//! Helper frames and observation-only callbacks for the native producer toolkit.
use crate::{Result, cli::Cli, helper::Helper, protocol::Frame};
use jackstay::{
    acquisition::arena::{ArenaConfig, FrameDescriptor},
    input::{Config, Outcome, Work},
    local::{Endpoint, Scope, Transport},
    model::{ClockDomain, DamageKind, FrameSyncKind, PixelFormat},
};
use jackstay_producer::{Builder, Producer};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc::RecvTimeoutError,
    },
    time::{Duration, Instant, SystemTime},
};

struct Observation {
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
    // No snapshots or affordance actions: the trait defaults are observation-only.
}

pub struct Source {
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
        let source = Builder::new(
            endpoint,
            ArenaConfig {
                resource_capacity: 8,
                retained_history: 1,
                producer_reserve: 1,
                payload_capacity: width as usize * height as usize * 4,
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
        *self.latest.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(jackstay_producer::Frame {
                descriptor: FrameDescriptor {
                    sequence: self.sequence,
                    timestamp_ns: self.started.elapsed().as_nanos() as u64,
                    width: header.width,
                    height: header.height,
                    stride: header.stride,
                    pixel_format: PixelFormat::Rgba8Unorm as u32,
                    clock_domain: ClockDomain::MediaTime as u32,
                    sync_kind: FrameSyncKind::CpuCopyComplete as u32,
                    damage_kind: DamageKind::FullFrame as u32,
                    ..FrameDescriptor::default()
                },
                bytes: frame.pixels,
            });
        Ok(())
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
    let mut helper = Helper::spawn(&mut cli.helper_command()?)?;
    println!("{path}");
    eprintln!("luchs: source ready: {path}");
    let mut last_poll = Instant::now();
    let mut received = 0;
    while !stop.load(Ordering::Relaxed) {
        match helper.receive(Duration::from_millis(50)) {
            Ok(frame) => {
                source.publish(frame?)?;
                received += 1;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                helper.finish()?;
                if received == 0 {
                    return Err("renderer exited without a frame".into());
                }
                break;
            }
        }
        if last_poll.elapsed() >= Duration::from_millis(250) {
            if let Some(path) = &watched {
                let now = modification_time(path);
                // Keep the last known time across an atomic-save disappearance.
                if now.is_some() && now != modified {
                    helper.reload()?;
                    modified = now;
                }
            }
            last_poll = Instant::now();
        }
    }
    eprintln!("luchs: stopped after {received} frames");
    match source.stop() {
        // Zero-capability input never acquires held state. Rejecting cleanup is
        // expected for this observation-only producer, not a failed CLI run.
        // jackstay-producer at ed785976 returns io::Error without a typed
        // cleanup variant. This exact message is pinned by the CLI test.
        // Typed error follow-up: https://github.com/flotilla-org/jackstay/issues/70
        Err(error) if error.to_string() == "input cleanup failed" => {
            eprintln!("luchs: shutdown: {error}");
        }
        result => result?,
    }
    Ok(())
}
