//! Helper frames and observation-only callbacks for the native producer toolkit.
use crate::{
    Result,
    capture::CapturePolicy,
    cli::Cli,
    helper::{CommandOutcome, Helper},
    protocol::Frame,
};
use jackstay::{
    acquisition::arena::{ArenaConfig, FrameDescriptor},
    input::{Config, Outcome, Work},
    local::{Endpoint, Scope, Transport},
    model::{ClockDomain, DamageKind, FrameSyncKind, PixelFormat},
};
use jackstay_producer::{Builder, Producer};
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    os::unix::fs::PermissionsExt,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc::RecvTimeoutError,
    },
    time::{Duration, Instant, SystemTime},
};

struct Observation {
    recycled: Arc<Mutex<Vec<Vec<u8>>>>,
    presentation: Arc<Mutex<Option<jackstay::affordances::Presentation>>>,
    logical_size: (f64, f64),
    latest: Arc<Mutex<Option<jackstay_producer::Frame>>>,
}
impl Producer for Observation {
    fn frame(&mut self) -> Option<jackstay_producer::Frame> {
        // Only an Option is swapped under this lock; unwinding cannot leave a
        // partially mutated frame or ownership bookkeeping to repair.
        self.latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn execute(&mut self, _work: Work) -> Outcome {
        Outcome::Unsupported
    }
    fn recycle(&mut self, frame: jackstay_producer::Frame) {
        let mut pool = self.recycled.lock().unwrap();
        if pool.len() < 4 {
            pool.push(frame.bytes);
        }
    }
    fn input_size(&mut self, _width: u32, _height: u32) -> (f64, f64) {
        self.logical_size
    }
    fn affordance(&mut self, event: jackstay::affordances::Event) {
        use jackstay::affordances::{Domain, Event, Presentation, Snapshot};
        let hint = match event {
            Event::Snapshot(Snapshot::Presentation(hint)) => hint,
            Event::Snapshot(Snapshot::Withdraw(Domain::Presentation)) | Event::Closed => {
                Presentation::default()
            }
            _ => return,
        };
        *self.presentation.lock().unwrap() = Some(hint);
    }
}

pub struct Source {
    recycled: Arc<Mutex<Vec<Vec<u8>>>>,
    presentation: Arc<Mutex<Option<jackstay::affordances::Presentation>>>,
    source: jackstay_producer::Source,
    latest: Arc<Mutex<Option<jackstay_producer::Frame>>>,
    started: Instant,
    sequence: u64,
}
impl Source {
    pub fn bind(name: &str, width: u32, height: u32) -> Result<(Self, String)> {
        crate::protocol::validate_size(width, height)?;
        let endpoint = Endpoint::new(Scope::User, name, Transport::LocalStream)?;
        let path = endpoint.render()?;
        let latest = Arc::new(Mutex::new(None));
        let recycled = Arc::new(Mutex::new(Vec::with_capacity(4)));
        let presentation = Arc::new(Mutex::new(None));
        let source = Builder::new(
            endpoint,
            ArenaConfig {
                resource_capacity: 8,
                retained_history: 1,
                producer_reserve: 1,
                payload_capacity: width as usize * height as usize * 4,
                // Frames are capped at 64 MiB; 1 GiB bounds aggregate arena allocations.
                memory_budget: 1024 * 1024 * 1024,
                max_incarnations: 3,
                drain_timeout: Duration::from_secs(5),
            },
            Config {
                // ABI 0.12 rejects modes=0. Advertise no capabilities and reject
                // every callback; the default cooperative mode conveys no authority.
                capabilities: 0,
                geometry: jackstay::input::Geometry {
                    revision: 1,
                    width: width.into(),
                    height: height.into(),
                },
                ..Config::default()
            },
            Observation {
                recycled: recycled.clone(),
                presentation: presentation.clone(),
                logical_size: (f64::from(width), f64::from(height)),
                latest: latest.clone(),
            },
        )
        .max_connections(16)
        .start()?;
        // Builder starts accepting before this chmod, but Jackstay creates and
        // verifies an owner-only (0700) runtime directory before binding. That
        // parent protects the socket regardless of the launcher's umask.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Ok((
            Self {
                recycled,
                presentation,
                source,
                latest,
                started: Instant::now(),
                sequence: 0,
            },
            path,
        ))
    }

    pub fn publish(&mut self, frame: Frame) -> Result<()> {
        if self.source.is_finished() {
            return Err("source pump stopped".into());
        }

        self.sequence += 1;
        let header = &frame.header;
        let previous = self
            .latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .replace(jackstay_producer::Frame {
                descriptor: FrameDescriptor {
                    sequence: self.sequence,
                    timestamp_ns: self.started.elapsed().as_nanos() as u64,
                    width: header.width,
                    height: header.height,
                    stride: header.stride,
                    pixel_format: PixelFormat::Bgra8Unorm as u32,
                    clock_domain: ClockDomain::MediaTime as u32,
                    sync_kind: FrameSyncKind::CpuCopyComplete as u32,
                    damage_kind: DamageKind::FullFrame as u32,
                    ..FrameDescriptor::default()
                },
                bytes: frame.pixels,
            });
        if let Some(previous) = previous {
            let mut pool = self.recycled.lock().unwrap();
            if pool.len() < 4 {
                pool.push(previous.bytes);
            }
        }
        Ok(())
    }

    pub fn presentation(&self) -> Option<jackstay::affordances::Presentation> {
        self.presentation.lock().unwrap().take()
    }

    pub fn recycle_into(&self, helper: &Helper) {
        for pixels in self.recycled.lock().unwrap().drain(..) {
            helper.recycle(pixels);
        }
    }

    pub fn stop(self) -> Result<()> {
        // EOF can follow the final helper frame immediately. Let the pump take
        // that frame before stopping; joining it completes that publication.
        // The pinned toolkit pump calls frame() unconditionally, even without
        // peers. Bound this flush to one second in case that contract changes;
        // ordered toolkit shutdown still runs if the final frame is skipped.
        // Upstream flush barrier: https://github.com/flotilla-org/jackstay/issues/71
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline
            && !self.source.is_finished()
            && self
                .latest
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some()
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.source.stop()?;
        Ok(())
    }
}

fn modification_time(path: &std::path::Path) -> Option<SystemTime> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

#[derive(Default)]
struct CaptureStats {
    snapshots: u64,
    skipped: u64,
    snapshot_ns: u128,
    publish_ns: u128,
}
impl CaptureStats {
    fn poll(
        &mut self,
        pending: &mut Option<crate::helper::PendingCommand>,
        policy: &mut CapturePolicy,
    ) -> Result<()> {
        let Some(command) = pending.as_ref() else {
            return Ok(());
        };
        if let Some(ack) = command.poll() {
            if ack.outcome != crate::protocol::AckOutcome::Executed {
                return Err("renderer capture failed".into());
            }
            if let Some(report) = ack.capture {
                self.snapshots += 1;
                self.skipped += u64::from(!report.published);
                self.snapshot_ns += u128::from(report.snapshot_ns);
                self.publish_ns += u128::from(report.publish_ns);
                policy.completed(report.published, Instant::now());
            } else {
                policy.completed(true, Instant::now());
            }
            *pending = None;
        } else if command.expired() {
            return Err("renderer capture timed out".into());
        }
        Ok(())
    }
    fn log(&self, received: u64) {
        eprintln!(
            "luchs: stats snapshots={} published={received} skipped={} mean_snapshot_ms={:.3} mean_publish_ms={:.3}",
            self.snapshots,
            self.skipped,
            self.snapshot_ns as f64 / self.snapshots.max(1) as f64 / 1e6,
            self.publish_ns as f64 / (self.snapshots - self.skipped).max(1) as f64 / 1e6
        );
    }
}

pub fn run(cli: Cli, stop: Arc<AtomicBool>) -> Result<()> {
    let name = cli
        .endpoint
        .clone()
        .unwrap_or_else(|| format!("luchs-{}", std::process::id()));
    let (mut source, path) = Source::bind(&name, cli.size.0, cli.size.1)?;
    // Capture the baseline before exposing readiness or starting the helper;
    // an edit after startup must not become the baseline and miss its reload.
    let watched = cli.local_page().filter(|_| cli.watch);
    let mut modified = watched.as_deref().and_then(modification_time);
    let changed = Arc::new(AtomicBool::new(false));
    let activity = changed.clone();
    let mut helper = Helper::spawn_with_state(&mut cli.helper_command()?, move |state| {
        if state
            .get("capture_changed")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            activity.store(true, Ordering::Release);
        }
    })?;
    let mut policy = CapturePolicy::new(cli.fps, Instant::now());
    let mut pending_capture: Option<crate::helper::PendingCommand> = None;
    let mut fingerprint = None;
    let mut stats = CaptureStats::default();
    println!("{path}");
    eprintln!("luchs: source ready: {path}");
    let mut last_poll = Instant::now();
    let mut received = 0;
    let mut last_reload_failure = None;
    while !stop.load(Ordering::Relaxed) {
        match helper.receive(Duration::from_millis(5)) {
            Ok(frame) => {
                let frame = frame?;
                let mut hash = DefaultHasher::new();
                (frame.header.width, frame.header.height, frame.header.stride).hash(&mut hash);
                frame.pixels.hash(&mut hash);
                let value = hash.finish();
                if policy.visible && fingerprint != Some(value) {
                    fingerprint = Some(value);
                    source.publish(frame)?;
                    received += 1;
                } else {
                    helper.recycle(frame.pixels);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                helper.finish()?;
                stats.poll(&mut pending_capture, &mut policy)?;
                if received == 0 {
                    return Err("renderer exited without a frame".into());
                }
                break;
            }
        }
        source.recycle_into(&helper);
        if changed.swap(false, Ordering::AcqRel) {
            policy.wake(Instant::now());
        }
        if let Some(hint) = source.presentation() {
            // Reject a hint that cannot fit a capture buffer before sending it.
            let width = (cli.size.0 as f64 * hint.scale).round();
            let height = (cli.size.1 as f64 * hint.scale).round();
            if width >= 1.0
                && height >= 1.0
                && width * height * 4.0 <= crate::protocol::MAX_FRAME_BYTES as f64
            {
                let outcome = helper.send_json_command(serde_json::json!({"type": "presentation", "visible": hint.visible, "scale": hint.scale}), crate::helper::COMMAND_TIMEOUT)?.wait();
                if outcome != CommandOutcome::Executed {
                    return Err(format!("renderer presentation: {outcome:?}").into());
                }
                if hint.visible && (!policy.visible || hint.scale != policy.scale) {
                    fingerprint = None;
                }
                policy.presentation(hint.visible, hint.scale, Instant::now());
            } else {
                eprintln!("luchs: ignoring scale hint beyond capture limits");
            }
        }
        stats.poll(&mut pending_capture, &mut policy)?;
        if pending_capture.is_none() && !helper.ended() && policy.due(Instant::now()) {
            pending_capture = Some(helper.send_command("capture", Duration::from_secs(10))?);
        }
        if last_poll.elapsed() >= Duration::from_millis(250) {
            if let Some(path) = &watched {
                let now = modification_time(path);
                // Keep the last known time across an atomic-save disappearance.
                if now.is_some() && now != modified {
                    match helper.reload()? {
                        CommandOutcome::Executed => {
                            policy.wake(Instant::now());
                            modified = now;
                            last_reload_failure = None;
                        }
                        CommandOutcome::Unsupported => {
                            return Err("renderer reload: unsupported".into());
                        }
                        outcome => {
                            if last_reload_failure.as_ref() != Some(&outcome) {
                                eprintln!("luchs: renderer reload: {outcome:?}; retrying");
                                last_reload_failure = Some(outcome);
                            }
                        }
                    }
                }
            }
            last_poll = Instant::now();
        }
    }
    eprintln!("luchs: stopped after {received} frames");
    stats.poll(&mut pending_capture, &mut policy)?;
    if cli.stats {
        stats.log(received);
    }
    let dropped = helper.dropped_frames();
    if dropped > 0 {
        eprintln!("luchs: helper dropped {dropped} frames");
    }
    let ignored = helper.ignored_acks();
    if ignored > 0 {
        eprintln!("luchs: helper ignored {ignored} unmatched or late acks");
    }
    match source.stop() {
        // Zero-capability input never acquires held state. Rejecting cleanup is
        // expected for this observation-only producer, not a failed CLI run.
        // The pinned jackstay-producer returns io::Error without a typed
        // cleanup variant. This exact message is pinned by the CLI test.
        // TODO(jackstay#70): replace the message comparison with a typed cleanup error.
        // https://github.com/flotilla-org/jackstay/issues/70
        Err(error) if error.to_string() == "input cleanup failed" => {
            eprintln!("luchs: shutdown: {error}");
        }
        result => result?,
    }
    Ok(())
}
