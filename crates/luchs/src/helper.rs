use std::{
    collections::{HashMap, VecDeque},
    io::{self, BufReader, Write},
    os::fd::AsRawFd,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::protocol::{Ack, AckOutcome, Frame, Record, encode_command, read_record};

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
    error: Option<io::Error>,
    ended: bool,
    pending: HashMap<u64, AckWaiter>,
}

#[derive(Default)]
struct Shared {
    state: Mutex<StreamState>,
    ready: Condvar,
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

    pub fn wait(self) -> CommandOutcome {
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

/// Owns the process and a single stdout demultiplexer. Frames never block ack
/// dispatch: the bounded two-frame mailbox drops the oldest on overflow.
pub struct Helper {
    child: Arc<Mutex<Child>>,
    stdin: Option<ChildStdin>,
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
    pub fn spawn_with_state(
        command: &mut Command,
        mut state_event: impl FnMut(serde_json::Map<String, serde_json::Value>) + Send + 'static,
    ) -> io::Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = child.stdin.take().expect("piped stdin");
        // Bound command writes too, including a helper that stops reading stdin.
        // SAFETY: stdin is a live, owned pipe descriptor; fcntl does not retain it.
        let flags = unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            let error = io::Error::last_os_error();
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let child = Arc::new(Mutex::new(child));
        let shared = Arc::new(Shared::default());
        let stream = shared.clone();
        let process = child.clone();
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_record(&mut reader) {
                    Ok(Some(Record::Frame(frame))) => {
                        let mut state = stream.state.lock().unwrap();
                        if state.frames.len() == 2 {
                            state.frames.pop_front();
                        }
                        state.frames.push_back(frame);
                        stream.ready.notify_one();
                    }
                    Ok(Some(Record::Ack(ack))) => {
                        if let Some(waiter) = stream.state.lock().unwrap().pending.remove(&ack.id) {
                            if Instant::now() <= waiter.deadline {
                                let _ = waiter.send.try_send(ack);
                            }
                        }
                    }
                    Ok(Some(Record::State(event))) => state_event(event),
                    result => {
                        if result.is_err() {
                            // A protocol violation is fatal even if nobody is
                            // currently receiving frames or awaiting commands.
                            let mut child = process.lock().unwrap();
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                        let mut state = stream.state.lock().unwrap();
                        state.error = result.err();
                        state.ended = true;
                        state.pending.clear();
                        stream.ready.notify_all();
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child,
            stdin: Some(stdin),
            shared,
            reader: Some(reader),
            next_id: 1,
        })
    }

    pub fn receive(&self, timeout: Duration) -> Result<io::Result<Frame>, RecvTimeoutError> {
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
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(RecvTimeoutError::Timeout);
            }
            state = self.shared.ready.wait_timeout(state, left).unwrap().0;
        }
    }

    pub fn send_command(&mut self, kind: &str, timeout: Duration) -> io::Result<PendingCommand> {
        let id = self.next_id;
        let next = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("command ids exhausted"))?;
        let bytes = encode_command(id, kind)?;
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
            self.stdin.as_mut().expect("running helper"),
            &bytes,
            deadline,
        ) {
            // A partial command cannot be retried on the same byte stream.
            let mut child = self.child.lock().unwrap();
            let _ = child.kill();
            let _ = child.wait();
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
                return Err(io::Error::other("renderer closed stdout without exiting"));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn write_command(stdin: &mut ChildStdin, mut bytes: &[u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "command write timed out",
            ));
        }
        match stdin.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "helper stdin closed",
                ));
            }
            Ok(len) => bytes = &bytes[len..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(1));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

impl Drop for Helper {
    fn drop(&mut self) {
        self.stdin.take();
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
        let _ = child.kill();
        let _ = child.wait();
        drop(child);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
