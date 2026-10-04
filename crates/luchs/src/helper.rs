use std::{
    collections::{HashMap, VecDeque},
    io::{self, BufReader, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{net::UnixStream, process::CommandExt},
    },
    panic::{AssertUnwindSafe, catch_unwind},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::protocol::{Ack, AckOutcome, Frame, Record, encode_json_command, read_record_reusing};
use rustix::event::{PollFd, PollFlags, Timespec, poll};

pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(1);
pub const MAX_PENDING_COMMANDS: usize = 64;

#[derive(Debug, PartialEq, Eq)]
pub enum CommandOutcome {
    Executed,
    Unsupported,
    Failed(Option<String>),
    Uncertain,
}

impl CommandOutcome {
    pub fn execution_outcome(&self) -> jackstay::input::Outcome {
        use jackstay::input::Outcome;
        match self {
            Self::Executed => Outcome::Executed,
            Self::Unsupported => Outcome::Unsupported,
            Self::Failed(_) => Outcome::Rejected,
            Self::Uncertain => Outcome::Uncertain,
        }
    }
}

struct AckWaiter {
    send: SyncSender<Ack>,
    deadline: Instant,
}

#[derive(Default)]
struct StreamState {
    frames: VecDeque<Frame>,
    buffers: Vec<Vec<u8>>,
    dropped_frames: u64,
    ignored_acks: u64,
    error: Option<io::Error>,
    ended: bool,
    wake_pending: bool,
    pending: HashMap<u64, AckWaiter>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<StreamState>,
    ready: Condvar,
}

/// Interrupt the event-aware receive wait after a host-side presentation change.
#[derive(Clone)]
pub struct Wake(Arc<Shared>);
impl Wake {
    pub fn notify(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.wake_pending = true;
        self.0.ready.notify_all();
    }
}

/// A per-id waiter. Dropping or timing it out releases its bounded slot; a
/// later ack cannot change the returned outcome or satisfy another command.
pub struct PendingCommand {
    id: u64,
    ack: Receiver<Ack>,
    shared: Arc<Shared>,
    deadline: Instant,
}

impl PendingCommand {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn poll(&self) -> Option<crate::protocol::Ack> {
        self.ack.try_recv().ok()
    }

    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    pub fn expired(&self) -> bool {
        self.remaining().is_zero()
    }

    pub fn wait(self) -> CommandOutcome {
        // The reader timestamps acceptance against this same deadline. A queued
        // ack accepted in time stays valid even if wait() is called later;
        // recv_timeout bounds how long we wait for an ack not yet received.
        match self
            .ack
            .recv_timeout(self.deadline.saturating_duration_since(Instant::now()))
        {
            Ok(ack) => match ack.outcome {
                AckOutcome::Executed => CommandOutcome::Executed,
                AckOutcome::Unsupported => CommandOutcome::Unsupported,
                AckOutcome::Failed => CommandOutcome::Failed(ack.detail),
            },
            Err(_) => CommandOutcome::Uncertain,
        }
    }
}

impl Drop for PendingCommand {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().pending.remove(&self.id);
    }
}

/// Owns the process and a single socket demultiplexer. Frames never block ack
/// dispatch: the bounded two-frame mailbox drops the oldest on overflow.
pub struct Helper {
    child: Arc<Mutex<Child>>,
    socket: Option<UnixStream>,
    shared: Arc<Shared>,
    reader: Option<JoinHandle<()>>,
    next_id: u64,
}

impl Helper {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        Self::spawn_with_state(command, |_| {})
    }

    /// The callback runs on the reader thread and must return promptly. State
    /// is an opaque JSON object until the affordance slice defines its schema.
    /// A callback panic terminates/reaps the helper and reports a stream error.
    pub fn spawn_with_state(
        command: &mut Command,
        mut state_event: impl FnMut(serde_json::Map<String, serde_json::Value>) + Send + 'static,
    ) -> io::Result<Self> {
        let (socket, inherited) = UnixStream::pair()?;
        socket.set_nonblocking(true)?;
        let input = socket.try_clone()?;
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
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()?;
        drop(inherited);
        let child = Arc::new(Mutex::new(child));
        let shared = Arc::new(Shared::default());
        let stream = shared.clone();
        let process = child.clone();
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(BlockingRead(input));
            let mut buffers = Vec::new();
            loop {
                buffers.append(&mut stream.state.lock().unwrap().buffers);
                buffers.truncate(4);
                match read_record_reusing(&mut reader, &mut buffers) {
                    Ok(Some(Record::Frame(frame))) => {
                        let mut state = stream.state.lock().unwrap();
                        if state.frames.len() == 2 {
                            if let Some(frame) = state.frames.pop_front() {
                                buffers.push(frame.pixels);
                            }
                            state.dropped_frames = state.dropped_frames.saturating_add(1);
                        }
                        state.frames.push_back(frame);
                        stream.ready.notify_one();
                    }
                    Ok(Some(Record::Ack(ack))) => {
                        let mut state = stream.state.lock().unwrap();
                        match state.pending.remove(&ack.id) {
                            Some(waiter) if Instant::now() <= waiter.deadline => {
                                let _ = waiter.send.try_send(ack);
                            }
                            _ => {
                                state.ignored_acks = state.ignored_acks.saturating_add(1);
                            }
                        }
                        state.wake_pending = true;
                        stream.ready.notify_all();
                    }
                    Ok(Some(Record::State(event))) => {
                        if catch_unwind(AssertUnwindSafe(|| state_event(event))).is_err() {
                            end_stream(
                                &stream,
                                &process,
                                Some(io::Error::other("state callback panicked")),
                            );
                            break;
                        }
                        Wake(stream.clone()).notify();
                    }
                    result => {
                        end_stream(&stream, &process, result.err());
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child,
            socket: Some(socket),
            shared,
            reader: Some(reader),
            next_id: 1,
        })
    }

    pub fn wake_handle(&self) -> Wake {
        Wake(self.shared.clone())
    }

    pub fn receive(&self, timeout: Duration) -> Result<io::Result<Frame>, RecvTimeoutError> {
        self.receive_inner(timeout, false)
    }

    /// Return Timeout early for an ack, state or host notification, so the
    /// scheduler can re-evaluate deadlines without polling between captures.
    pub fn receive_event(&self, timeout: Duration) -> Result<io::Result<Frame>, RecvTimeoutError> {
        self.receive_inner(timeout, true)
    }

    fn receive_inner(
        &self,
        timeout: Duration,
        events: bool,
    ) -> Result<io::Result<Frame>, RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.state.lock().unwrap();
        loop {
            if let Some(error) = state.error.take() {
                return Ok(Err(error));
            }
            if let Some(frame) = state.frames.pop_front() {
                return Ok(Ok(frame));
            }
            if state.ended {
                return Err(RecvTimeoutError::Disconnected);
            }
            if events && std::mem::take(&mut state.wake_pending) {
                return Err(RecvTimeoutError::Timeout);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(RecvTimeoutError::Timeout);
            }
            state = self.shared.ready.wait_timeout(state, left).unwrap().0;
        }
    }

    pub fn ended(&self) -> bool {
        self.shared.state.lock().unwrap().ended
    }

    pub fn recycle(&self, pixels: Vec<u8>) {
        let mut state = self.shared.state.lock().unwrap();
        if state.buffers.len() < 4 {
            state.buffers.push(pixels);
        }
    }

    pub fn dropped_frames(&self) -> u64 {
        self.shared.state.lock().unwrap().dropped_frames
    }

    /// Counts unmatched and late acks, including helper duplicate replies.
    pub fn ignored_acks(&self) -> u64 {
        self.shared.state.lock().unwrap().ignored_acks
    }

    pub fn send_command(&mut self, kind: &str, timeout: Duration) -> io::Result<PendingCommand> {
        self.send_json_command(serde_json::json!({"type": kind}), timeout)
    }

    pub fn send_json_command(
        &mut self,
        mut command: serde_json::Value,
        timeout: Duration,
    ) -> io::Result<PendingCommand> {
        let id = self.next_id;
        let next = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("command ids exhausted"))?;
        let object = command.as_object_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "command must be an object")
        })?;
        object.insert("id".into(), id.into());
        let bytes = encode_json_command(&command)?;
        let (send, ack) = mpsc::sync_channel(1);
        let deadline = Instant::now() + timeout;
        {
            let mut state = self.shared.state.lock().unwrap();
            if state.ended {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "helper stopped"));
            }
            if state.pending.len() >= MAX_PENDING_COMMANDS {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "too many pending commands",
                ));
            }
            state.pending.insert(id, AckWaiter { send, deadline });
        }
        self.next_id = next;
        let pending = PendingCommand {
            id,
            ack,
            shared: self.shared.clone(),
            deadline,
        };
        if let Err(error) = write_command(
            self.socket.as_mut().expect("running helper"),
            &bytes,
            deadline,
        ) {
            // A peer that exits with unread commands can reset the socket on
            // Linux. Let the reader drain complete final frames and verify exit.
            if matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
            ) {
                return Err(error);
            }
            // A partial command cannot be retried on the same byte stream.
            end_stream(
                &self.shared,
                &self.child,
                Some(io::Error::new(error.kind(), error.to_string())),
            );
            return Err(error);
        }
        Ok(pending)
    }

    pub fn command(&mut self, kind: &str, timeout: Duration) -> CommandOutcome {
        self.send_command(kind, timeout)
            .map_or(CommandOutcome::Uncertain, PendingCommand::wait)
    }

    pub fn reload(&mut self) -> io::Result<CommandOutcome> {
        Ok(self.send_command("reload", COMMAND_TIMEOUT)?.wait())
    }

    pub fn finish(&mut self) -> io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(status) = self.child.lock().unwrap().try_wait()? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(io::Error::other(format!("renderer exited with {status}")))
                };
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other("renderer closed socket without exiting"));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn kill_and_reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn end_stream(shared: &Shared, process: &Mutex<Child>, error: Option<io::Error>) {
    if error.is_some() {
        // Fail immediately even when nobody is receiving frames or awaiting acks.
        kill_and_reap(&mut process.lock().unwrap());
    }
    let mut state = shared.state.lock().unwrap();
    // EOF can race a command-write failure; never erase its root cause.
    if error.is_some() {
        state.error = error;
    }
    state.ended = true;
    state.pending.clear();
    shared.ready.notify_all();
}

// dup'd socket descriptors share O_NONBLOCK. Poll only the reader thread,
// retaining bounded writes on all Unix platforms (Darwin ignores MSG_DONTWAIT).
struct BlockingRead(UnixStream);
impl Read for BlockingRead {
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

fn write_command(socket: &mut UnixStream, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "command write timed out",
            ));
        }
        match socket.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "helper socket closed",
                ));
            }
            Ok(len) => bytes = &bytes[len..],
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

impl Drop for Helper {
    fn drop(&mut self) {
        if let Some(socket) = self.socket.take() {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        let mut child = self.child.lock().unwrap();
        if matches!(child.try_wait(), Ok(None)) {
            // SAFETY: the unreaped child owns this PID.
            unsafe {
                libc::kill(child.id() as libc::pid_t, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_millis(500);
            while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
        }
        kill_and_reap(&mut child);
        drop(child);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
