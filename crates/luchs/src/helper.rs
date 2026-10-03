use std::{
    io::{self, BufReader, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::protocol::{Frame, read_frame};

/// Owns every child resource, including the bounded stdout reader.
pub struct Helper {
    child: Child,
    stdin: Option<ChildStdin>,
    frames: Option<Receiver<io::Result<Frame>>>,
    reader: Option<JoinHandle<()>>,
}

impl Helper {
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdout = child.stdout.take().expect("piped stdout");
        let stdin = child.stdin.take();
        let (send, frames) = mpsc::sync_channel(1);
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match read_frame(&mut reader) {
                    Ok(Some(frame)) => {
                        if send.send(Ok(frame)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = send.send(Err(error));
                        break;
                    }
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            frames: Some(frames),
            reader: Some(reader),
        })
    }

    pub fn receive(&self, timeout: Duration) -> Result<io::Result<Frame>, RecvTimeoutError> {
        self.frames
            .as_ref()
            .expect("running helper")
            .recv_timeout(timeout)
    }

    pub fn reload(&mut self) -> io::Result<()> {
        self.stdin
            .as_mut()
            .expect("running helper")
            .write_all(b"{\"type\":\"reload\"}\n")
    }

    pub fn finish(&mut self) -> io::Result<()> {
        // EOF can precede process exit slightly, but a helper closing stdout and
        // hanging must not make shutdown wait forever.
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(status) = self.child.try_wait()? {
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

impl Drop for Helper {
    fn drop(&mut self) {
        // Unblock a reader waiting to send before terminating/reaping the child.
        self.frames.take();
        self.stdin.take();
        if matches!(self.child.try_wait(), Ok(None)) {
            // SAFETY: the unreaped child owns this PID. SIGTERM also terminates
            // the unchanged Swift helper, whose stdin EOF is not a quit command.
            unsafe {
                libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_millis(500);
            while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
