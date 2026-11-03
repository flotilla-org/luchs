//! Windows helper transport: a private named pipe the helper opens by name,
//! payload objects duplicated into the connected helper and named in-band, and
//! a job object that bounds the helper's process tree.
//!
//! Nothing is inherited: Rust's `Command` makes every inheritable handle
//! visible to every concurrent spawn, so the helper connects to a fresh
//! Jackstay Local Endpoint instead. Only the helper process, or a process in
//! its job (a launcher such as `cmd.exe` may start the real helper), is
//! accepted; anything else of the same user that finds the pipe is refused.
use std::{
    io::{self, Read, Write},
    marker::PhantomData,
    os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle},
    process::{Child, Command, ExitStatus},
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use jackstay::local::{
    self, Endpoint, Listener, PipeStream, Scope, ShutdownHandle as PipeShutdown, Transport,
};
use windows_sys::Win32::{
    Foundation::{DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, WAIT_OBJECT_0},
    System::{
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
        },
        Pipes::DisconnectNamedPipe,
        Threading::{GetCurrentProcess, WaitForSingleObject},
    },
};

/// A payload object lent to [`super::CommandSender::send_with_payload`].
pub type Payload<'a> = BorrowedHandle<'a>;
/// A payload object as Jackstay exports it.
pub type OwnedPayload = OwnedHandle;

/// How long a started helper has to open its pipe.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub fn borrow(object: &OwnedPayload) -> Payload<'_> {
    object.as_handle()
}

fn check(result: i32) -> io::Result<()> {
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "helper pipe closed")
}

pub(super) struct Spawned {
    pub process: Process,
    pub reader: Reader,
    pub writer: Writer,
    pub link: Link,
}

/// Start the helper with a private pipe name in `LUCHS_HELPER_PIPE` and wait
/// until it connects. A helper that exits first yields a closed stream, as a
/// Unix helper that exits without writing yields EOF.
pub(super) fn spawn(command: &mut Command) -> io::Result<Spawned> {
    let job = job()?;
    let (listener, name) = listen()?;
    command.env("LUCHS_HELPER_PIPE", name);
    let mut process = Process {
        child: command.spawn()?,
        job,
    };
    // SAFETY: both handles are live; the job keeps no reference to the child handle.
    let assigned = check(unsafe {
        AssignProcessToJobObject(process.job.as_raw_handle(), process.child.as_raw_handle())
    });
    let connected = assigned.and_then(|()| accept(&listener, &process));
    drop(listener);
    let connected = match connected {
        Ok(connected) => connected,
        Err(error) => {
            process.kill_and_reap();
            return Err(error);
        }
    };
    let Some((stream, peer)) = connected else {
        return Ok(Spawned {
            process,
            reader: Reader(None),
            writer: Writer(None),
            link: Link(None),
        });
    };
    let pipe = stream.into_handle()?;
    let control = pipe.try_clone()?;
    // SAFETY: each stream exclusively owns its own duplicate of this overlapped
    // server handle; overlapped reads and writes on one pipe may run concurrently.
    let reader = unsafe { PipeStream::from_owned_handle(pipe.try_clone()?, true)? };
    // SAFETY: as above.
    let writer = unsafe { PipeStream::from_owned_handle(pipe, true)? };
    let shutdown = [reader.shutdown_handle(), writer.shutdown_handle()];
    Ok(Spawned {
        process,
        reader: Reader(Some(reader)),
        writer: Writer(Some((writer, peer))),
        link: Link(Some((control, shutdown))),
    })
}

/// A job that dies with this process and takes the helper's tree with it.
fn job() -> io::Result<OwnedHandle> {
    // SAFETY: no attributes or name; a non-null result is a fresh owned handle.
    let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: raw is a fresh job handle owned by nothing else.
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: the structure matches the information class and outlives the call.
    check(unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    })?;
    Ok(job)
}

/// Bind a fresh user-scoped endpoint; returns it with its `\\.\pipe\` name.
fn listen() -> io::Result<(Listener, String)> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut attempts = 0;
    loop {
        let name = format!(
            "luchs-helper-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let endpoint = Endpoint::new(Scope::User, &name, Transport::LocalStream)?;
        match Listener::bind(&endpoint) {
            Ok(listener) => return Ok((listener, endpoint.render()?)),
            // Never share a name someone else created; pick the next one.
            Err(local::Error::InUse) if attempts < 8 => attempts += 1,
            Err(local::Error::Io(error)) => return Err(error),
            Err(error) => return Err(io::Error::other(error)),
        }
    }
}

/// Accept the helper's connection. `None`: the helper exited without connecting.
fn accept(
    listener: &Listener,
    process: &Process,
) -> io::Result<Option<(PipeStream, Arc<OwnedHandle>)>> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let child = process.child.as_handle();
    let job = process.job.as_handle();
    let pid = process.child.id();
    let done = AtomicBool::new(false);
    thread::scope(|scope| {
        // Wake the accept once the helper can no longer connect.
        let watcher = scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                // SAFETY: the child handle outlives this scope.
                if unsafe { WaitForSingleObject(child.as_raw_handle(), 10) } == WAIT_OBJECT_0 {
                    listener.cancel();
                    return false;
                }
                if Instant::now() >= deadline {
                    listener.cancel();
                    return true;
                }
            }
            false
        });
        let accepted = loop {
            match listener.accept() {
                Ok(connection) => {
                    let peer_pid = connection.peer().pid;
                    let stream = connection.into_stream();
                    // A peer that left during identification cannot be the helper's
                    // live connection; keep waiting.
                    let Ok(peer) = stream.peer_process() else {
                        continue;
                    };
                    match in_job(&peer, job) {
                        Ok(true) => break Ok(Some((stream, peer))),
                        _ if peer_pid == pid => break Ok(Some((stream, peer))),
                        // Another process of this user found the pipe: refuse it.
                        _ => {}
                    }
                }
                Err(local::Error::Cancelled) => break Ok(None),
                Err(local::Error::RefusedPeer(_)) => {}
                Err(local::Error::Io(error)) => break Err(error),
                Err(error) => break Err(io::Error::other(error)),
            }
        };
        done.store(true, Ordering::Release);
        let timed_out = watcher.join().unwrap_or(false);
        match accepted {
            Ok(None) if timed_out => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "renderer did not connect to its helper pipe",
            )),
            accepted => accepted,
        }
    })
}

fn in_job(process: &OwnedHandle, job: BorrowedHandle<'_>) -> io::Result<bool> {
    let mut member = 0;
    // SAFETY: both handles are live and the output is writable.
    check(unsafe { IsProcessInJob(process.as_raw_handle(), job.as_raw_handle(), &mut member) })?;
    Ok(member != 0)
}

pub(super) struct Process {
    child: Child,
    job: OwnedHandle,
}

impl Process {
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    /// Windows has no `SIGTERM` for a windowless process. [`Link::shutdown`]
    /// already disconnected the pipe, which is the helper's request to exit.
    pub fn request_stop(&mut self) {}

    /// Terminate the helper's whole job, then reap the direct child.
    pub fn kill_and_reap(&mut self) {
        // SAFETY: the job handle is live.
        unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) };
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Disconnects the pipe: the helper sees EOF and local I/O stops.
pub(super) struct Link(Option<(OwnedHandle, [PipeShutdown; 2])>);

impl Link {
    pub fn shutdown(&self) {
        let Some((pipe, streams)) = &self.0 else {
            return;
        };
        // SAFETY: the server pipe handle is live.
        unsafe { DisconnectNamedPipe(pipe.as_raw_handle()) };
        for stream in streams {
            stream.shutdown();
        }
    }
}

pub(super) struct Reader(Option<PipeStream>);

impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        match &mut self.0 {
            Some(stream) => stream.read(bytes),
            None => Ok(0),
        }
    }
}

/// A payload handle duplicated into the helper. It is closed there again
/// unless the command naming it was written completely: the helper cannot
/// have learned its value from a partial record.
pub(super) struct Attachment<'a> {
    peer: Arc<OwnedHandle>,
    value: usize,
    delivered: bool,
    _object: PhantomData<BorrowedHandle<'a>>,
}

impl Drop for Attachment<'_> {
    fn drop(&mut self) {
        if !self.delivered {
            // SAFETY: closes our unused duplicate in the helper; no target.
            unsafe {
                DuplicateHandle(
                    self.peer.as_raw_handle(),
                    self.value as _,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    0,
                    0,
                    DUPLICATE_CLOSE_SOURCE,
                )
            };
        }
    }
}

pub(super) struct Writer(Option<(PipeStream, Arc<OwnedHandle>)>);

impl Writer {
    /// Duplicate `object` into the helper and name it as `"handle"` in the command.
    pub fn attach<'a>(
        &self,
        command: &mut serde_json::Map<String, serde_json::Value>,
        object: Payload<'a>,
    ) -> io::Result<Attachment<'a>> {
        let (_, peer) = self.0.as_ref().ok_or_else(closed)?;
        let mut value = ptr::null_mut();
        // SAFETY: the source handle is borrowed and live; the peer handle has
        // PROCESS_DUP_HANDLE. The duplicate is not inheritable.
        check(unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                object.as_raw_handle(),
                peer.as_raw_handle(),
                &mut value,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        })?;
        let attachment = Attachment {
            peer: peer.clone(),
            value: value as usize,
            delivered: false,
            _object: PhantomData,
        };
        command.insert("handle".into(), (attachment.value as u64).into());
        Ok(attachment)
    }

    pub fn write(
        &mut self,
        mut bytes: &[u8],
        deadline: Instant,
        attachment: Option<Attachment<'_>>,
    ) -> io::Result<()> {
        let (stream, _) = self.0.as_mut().ok_or_else(closed)?;
        while !bytes.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "command write timed out",
                ));
            }
            // A timed-out overlapped write is cancelled and reports any bytes
            // the pipe accepted, so the outer loop enforces the full deadline.
            stream.set_write_timeout(Some(remaining))?;
            match stream.write(bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "helper socket closed",
                    ));
                }
                Ok(len) => bytes = &bytes[len..],
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(error),
            }
        }
        if let Some(mut attachment) = attachment {
            attachment.delivered = true;
        }
        Ok(())
    }
}
