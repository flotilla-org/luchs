//! Helper process ownership, command ids and acknowledgement routing over a
//! per-OS transport (see docs/helper-protocol.md): a Unix socketpair, or a
//! Windows named pipe. Everything above the transport is shared.
use std::{
    collections::HashMap,
    io::{self, BufReader},
    panic::{AssertUnwindSafe, catch_unwind},
    process::{Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::protocol::{Ack, AckOutcome, Record, encode_json_command, read_record};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as transport;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as transport;

pub use transport::{OwnedPayload, Payload, borrow as payload};
type Process = transport::Process;

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
            Self::Failed(_) => Outcome::Uncertain,
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
        match self.wait_ack() {
            Some(ack) => match ack.outcome {
                AckOutcome::Executed => CommandOutcome::Executed,
                AckOutcome::Unsupported => CommandOutcome::Unsupported,
                AckOutcome::Failed => CommandOutcome::Failed(ack.detail),
            },
            None => CommandOutcome::Uncertain,
        }
    }

    pub fn wait_ack(self) -> Option<Ack> {
        self.ack.recv_timeout(self.remaining()).ok()
    }
}

impl Drop for PendingCommand {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().pending.remove(&self.id);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum HelperEvent {
    Wake,
    Timeout,
    Closed,
}

/// Owns the process and a transport demultiplexer for acknowledgements and state.
pub struct Helper {
    child: Arc<Mutex<Process>>,
    link: Option<transport::Link>,
    shared: Arc<Shared>,
    reader: Option<JoinHandle<()>>,
    sender: CommandSender,
}

/// Cloneable command port; writes and IDs are serialized, acknowledgement waits
/// do not hold the writer lock. It cannot keep the helper process alive.
#[derive(Clone)]
pub struct CommandSender {
    writer: Arc<Mutex<CommandWriter>>,
    shared: Arc<Shared>,
    process: Arc<Mutex<Process>>,
}
struct CommandWriter {
    transport: transport::Writer,
    next_id: u64,
}

impl Helper {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        Self::spawn_with_state(command, |_| {})
    }

    /// The callback runs on the reader thread and must return promptly. State
    /// includes complete page-domain snapshots (see docs/helper-protocol.md).
    /// A callback panic terminates/reaps the helper and reports a stream error.
    pub fn spawn_with_state(
        command: &mut Command,
        mut state_event: impl FnMut(serde_json::Map<String, serde_json::Value>) + Send + 'static,
    ) -> io::Result<Self> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        let transport::Spawned {
            process,
            reader,
            writer,
            link,
        } = transport::spawn(command)?;
        let child = Arc::new(Mutex::new(process));
        let shared = Arc::new(Shared::default());
        let stream = shared.clone();
        let process = child.clone();
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            loop {
                match read_record(&mut reader) {
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
        let sender = CommandSender {
            process: child.clone(),
            writer: Arc::new(Mutex::new(CommandWriter {
                transport: writer,
                next_id: 1,
            })),
            shared: shared.clone(),
        };
        Ok(Self {
            child,
            link: Some(link),
            shared,
            reader: Some(reader),
            sender,
        })
    }

    pub fn wake_handle(&self) -> Wake {
        Wake(self.shared.clone())
    }

    /// Wait for an ack, state, host wake, deadline or stream closure.
    pub fn receive_event(&self, timeout: Duration) -> io::Result<HelperEvent> {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.state.lock().unwrap();
        loop {
            if let Some(error) = state.error.take() {
                return Err(error);
            }
            if state.ended {
                return Ok(HelperEvent::Closed);
            }
            if std::mem::take(&mut state.wake_pending) {
                return Ok(HelperEvent::Wake);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(HelperEvent::Timeout);
            }
            state = self.shared.ready.wait_timeout(state, left).unwrap().0;
        }
    }

    pub(crate) fn take_error(&self) -> Option<io::Error> {
        self.shared.state.lock().unwrap().error.take()
    }

    pub(crate) fn running(&self) -> io::Result<bool> {
        if self.ended() {
            return Ok(false);
        }
        Ok(self.child.lock().unwrap().try_wait()?.is_none())
    }

    pub fn ended(&self) -> bool {
        self.shared.state.lock().unwrap().ended
    }

    /// Counts unmatched and late acks, including helper duplicate replies.
    pub fn ignored_acks(&self) -> u64 {
        self.shared.state.lock().unwrap().ignored_acks
    }

    pub fn send_command(&mut self, kind: &str, timeout: Duration) -> io::Result<PendingCommand> {
        self.send_json_command(serde_json::json!({"type": kind}), timeout)
    }

    pub fn command_sender(&self) -> CommandSender {
        self.sender.clone()
    }

    pub fn send_json_command(
        &mut self,
        command: serde_json::Value,
        timeout: Duration,
    ) -> io::Result<PendingCommand> {
        self.sender.send_json_command(command, timeout)
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

impl CommandSender {
    pub(crate) fn ended(&self) -> bool {
        self.shared.state.lock().unwrap().ended
    }

    pub fn terminate(&self) {
        end_stream(
            &self.shared,
            &self.process,
            Some(io::Error::other("renderer terminated")),
        );
    }

    pub fn send_json_command(
        &self,
        command: serde_json::Value,
        timeout: Duration,
    ) -> io::Result<PendingCommand> {
        self.send_with_payload(command, timeout, None)
    }

    /// Send a command carrying one payload object, transferred as the
    /// platform's transport defines (docs/helper-protocol.md).
    pub fn send_with_payload(
        &self,
        mut command: serde_json::Value,
        timeout: Duration,
        payload: Option<Payload<'_>>,
    ) -> io::Result<PendingCommand> {
        let mut writer = self.writer.lock().unwrap();
        let id = writer.next_id;
        let next = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("command ids exhausted"))?;
        let object = command.as_object_mut().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "command must be an object")
        })?;
        object.insert("id".into(), id.into());
        let attachment = payload
            .map(|payload| writer.transport.attach(object, payload))
            .transpose()?;
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
        writer.next_id = next;
        let pending = PendingCommand {
            id,
            ack,
            shared: self.shared.clone(),
            deadline,
        };
        if let Err(error) = writer.transport.write(&bytes, deadline, attachment) {
            if matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
            ) {
                return Err(error);
            }
            end_stream(
                &self.shared,
                &self.process,
                Some(io::Error::new(error.kind(), error.to_string())),
            );
            return Err(error);
        }
        Ok(pending)
    }
}

fn end_stream(shared: &Shared, process: &Mutex<Process>, error: Option<io::Error>) {
    if error.is_some() {
        // Fail immediately even when nobody is receiving frames or awaiting acks.
        process.lock().unwrap().kill_and_reap();
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

impl Drop for Helper {
    fn drop(&mut self) {
        if let Some(link) = self.link.take() {
            link.shutdown();
        }
        let mut child = self.child.lock().unwrap();
        if matches!(child.try_wait(), Ok(None)) {
            child.request_stop();
            let deadline = Instant::now() + Duration::from_millis(500);
            while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
        }
        child.kill_and_reap();
        drop(child);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
