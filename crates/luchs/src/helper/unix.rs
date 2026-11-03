//! Unix helper transport: one inherited `AF_UNIX` socketpair, payload objects
//! attached as `SCM_RIGHTS`, and `SIGTERM` before the final kill.
use std::{
    io::{self, Read, Write},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    process::{Child, Command, ExitStatus},
    time::{Duration, Instant},
};

use rustix::event::{PollFd, PollFlags, Timespec, poll};

/// A payload object lent to [`super::CommandSender::send_with_payload`].
pub type Payload<'a> = BorrowedFd<'a>;
/// A payload object as Jackstay exports it.
pub type OwnedPayload = OwnedFd;

pub fn borrow(object: &OwnedPayload) -> Payload<'_> {
    object.as_fd()
}

pub(super) struct Spawned {
    pub process: Process,
    pub reader: Reader,
    pub writer: Writer,
    pub link: Link,
}

/// Start the helper with the child end of a fresh socketpair in `LUCHS_HELPER_FD`.
pub(super) fn spawn(command: &mut Command) -> io::Result<Spawned> {
    let (socket, inherited) = UnixStream::pair()?;
    socket.set_nonblocking(true)?;
    let input = socket.try_clone()?;
    let output = socket.try_clone()?;
    // Stdio setup may overwrite descriptors 0..=2 before pre_exec runs.
    // Duplicate above that range even when the launching process closed stdio.
    let inherited = {
        // SAFETY: fcntl duplicates a live descriptor; UnixStream owns the result.
        let fd = unsafe { libc::fcntl(inherited.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        unsafe { UnixStream::from_raw_fd(fd) }
    };
    let fd = inherited.as_raw_fd();
    command.env("LUCHS_HELPER_FD", fd.to_string());
    // SAFETY: only async-signal-safe fcntl runs between fork and exec. The
    // parent retains CLOEXEC; only this child's socket survives exec.
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    drop(inherited);
    Ok(Spawned {
        process: Process(child),
        reader: Reader(input),
        writer: Writer(output),
        link: Link(socket),
    })
}

pub(super) struct Process(Child);

impl Process {
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.0.try_wait()
    }

    /// Ask a running helper to exit before the final kill.
    pub fn request_stop(&mut self) {
        // SAFETY: the unreaped child owns this PID.
        unsafe {
            libc::kill(self.0.id() as libc::pid_t, libc::SIGTERM);
        }
    }

    pub fn kill_and_reap(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Closes both directions of the helper socket.
pub(super) struct Link(UnixStream);

impl Link {
    pub fn shutdown(&self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}

// dup'd socket descriptors share O_NONBLOCK. Poll only the reader thread,
// retaining bounded writes on all Unix platforms (Darwin ignores MSG_DONTWAIT).
pub(super) struct Reader(UnixStream);
impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.0.read(bytes) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    let mut fds = [PollFd::new(&self.0, PollFlags::IN)];
                    if let Err(error) = poll(&mut fds, None) {
                        if error != rustix::io::Errno::INTR {
                            return Err(error.into());
                        }
                    }
                }
                result => return result,
            }
        }
    }
}

/// A payload descriptor travelling as `SCM_RIGHTS` on the command's first byte.
pub(super) struct Attachment<'a>(BorrowedFd<'a>);

pub(super) struct Writer(UnixStream);

impl Writer {
    /// The descriptor travels out of band; the command JSON is unchanged.
    pub fn attach<'a>(
        &self,
        _command: &mut serde_json::Map<String, serde_json::Value>,
        object: Payload<'a>,
    ) -> io::Result<Attachment<'a>> {
        Ok(Attachment(object))
    }

    pub fn write(
        &mut self,
        bytes: &[u8],
        deadline: Instant,
        attachment: Option<Attachment<'_>>,
    ) -> io::Result<()> {
        write_command(&mut self.0, bytes, deadline, attachment.map(|a| a.0))
    }
}

fn write_command(
    socket: &mut UnixStream,
    mut bytes: &[u8],
    deadline: Instant,
    mut fd: Option<BorrowedFd<'_>>,
) -> io::Result<()> {
    while !bytes.is_empty() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "command write timed out",
            ));
        }
        let result = if let Some(object) = fd {
            send_fd(socket, bytes, object)
        } else {
            socket.write(bytes)
        };
        match result {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "helper socket closed",
                ));
            }
            Ok(len) => {
                fd = None;
                bytes = &bytes[len..];
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                // Keep each wait representable on platforms with millisecond
                // poll limits, while the outer loop enforces the full deadline.
                let remaining = deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(1));
                let timeout = Timespec::try_from(remaining).expect("at most one second");
                let mut fds = [PollFd::new(&*socket, PollFlags::OUT)];
                if let Err(error) = poll(&mut fds, Some(&timeout)) {
                    if error != rustix::io::Errno::INTR {
                        return Err(error.into());
                    }
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

// Aligned ancillary storage, one payload fd, attached to the first command byte.
fn send_fd(socket: &UnixStream, bytes: &[u8], fd: BorrowedFd<'_>) -> io::Result<usize> {
    // SAFETY: all message pointers stay live through sendmsg; CMSG_SPACE bounds
    // the aligned storage and CMSG_DATA points to one initialized descriptor.
    unsafe {
        let mut control = [0usize; 8];
        let mut iov = libc::iovec {
            iov_base: bytes.as_ptr().cast_mut().cast(),
            iov_len: bytes.len(),
        };
        let mut message: libc::msghdr = std::mem::zeroed();
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<libc::c_int>() as _) as _;
        let header = libc::CMSG_FIRSTHDR(&message);
        (*header).cmsg_level = libc::SOL_SOCKET;
        (*header).cmsg_type = libc::SCM_RIGHTS;
        (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<libc::c_int>() as _) as _;
        libc::CMSG_DATA(header)
            .cast::<libc::c_int>()
            .write(fd.as_raw_fd());
        #[cfg(target_os = "linux")]
        let flags = libc::MSG_NOSIGNAL;
        #[cfg(not(target_os = "linux"))]
        let flags = 0;
        let n = libc::sendmsg(socket.as_raw_fd(), &message, flags);
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }
}
