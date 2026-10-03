//! Endpoint lifecycle, CPU publication and shutdown, kept together for the
//! future Jackstay provider toolkit. Each consumer owns an independent worker.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::RecvTimeoutError,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime},
};

use jackstay::{
    acquisition::{
        arena::{ArenaConfig, ArenaProducer, FrameDescriptor, ReconfigurationStatus},
        socket::serve_cpu,
    },
    bootstrap,
    local::{self, Endpoint, Listener, Scope, Transport},
    model::{ClockDomain, DamageKind, FrameSyncKind, PixelFormat},
};

use crate::{Result, cli::Cli, helper::Helper, protocol::Frame};

type Shape = (u32, u32, u32);

pub struct Source {
    producer: Arc<Mutex<ArenaProducer>>,
    listener: Arc<Listener>,
    acceptor: Option<JoinHandle<()>>,
    shape: Shape,
    pending: Option<Shape>,
    started: Instant,
    sequence: u64,
}

impl Source {
    pub fn bind(name: &str, width: u32, height: u32) -> Result<(Self, String)> {
        crate::protocol::validate_size(width, height)?;
        let endpoint = Endpoint::new(Scope::User, name, Transport::LocalStream)?;
        let path = endpoint.render()?;
        let producer = Arc::new(Mutex::new(ArenaProducer::new(ArenaConfig {
            resource_capacity: 8,
            retained_history: 1,
            producer_reserve: 1,
            payload_capacity: width as usize * height as usize * 4,
            memory_budget: 1024 * 1024 * 1024,
            max_incarnations: 3,
            drain_timeout: Duration::from_secs(5),
        })?));
        let listener = Arc::new(Listener::bind(&endpoint)?);
        // The private parent already excludes other users during bind. Also
        // restrict the socket inode independently of the launcher's umask.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let acceptor = {
            let listener = listener.clone();
            let producer = producer.clone();
            thread::spawn(move || {
                let mut workers: Vec<(local::ShutdownHandle, JoinHandle<()>)> = Vec::new();
                loop {
                    let connection = match listener.accept() {
                        Ok(connection) => connection,
                        Err(local::Error::Cancelled) => break,
                        Err(error) => {
                            eprintln!("luchs: accept: {error}");
                            break;
                        }
                    };
                    let mut index = 0;
                    while index < workers.len() {
                        if workers[index].1.is_finished() {
                            let (_, worker) = workers.swap_remove(index);
                            let _ = worker.join();
                        } else {
                            index += 1;
                        }
                    }
                    // Bound idle handshakes as well as admitted consumers.
                    if workers.len() >= 16 {
                        continue;
                    }
                    let stream = connection.into_stream();
                    let shutdown = match local::shutdown_handle(&stream) {
                        Ok(shutdown) => shutdown,
                        Err(error) => {
                            eprintln!("luchs: consumer: {error}");
                            continue;
                        }
                    };
                    let producer = producer.clone();
                    let worker = thread::spawn(move || {
                        let result = (|| -> Result<()> {
                            let accepted = bootstrap::accept(stream, None)?;
                            serve_cpu(accepted.media, producer)?;
                            Ok(())
                        })();
                        if let Err(error) = result {
                            eprintln!("luchs: consumer disconnected: {error}");
                        }
                    });
                    workers.push((shutdown, worker));
                }
                for (shutdown, _) in &workers {
                    shutdown.shutdown();
                }
                for (_, worker) in workers {
                    let _ = worker.join();
                }
            })
        };
        Ok((
            Self {
                producer,
                listener,
                acceptor: Some(acceptor),
                shape: (width, height, width * 4),
                pending: None,
                started: Instant::now(),
                sequence: 0,
            },
            path,
        ))
    }

    pub fn publish(&mut self, frame: &Frame) -> Result<()> {
        if self
            .acceptor
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            return Err("source accept worker stopped".into());
        }
        let mut producer = self
            .producer
            .lock()
            .map_err(|_| "producer mutex poisoned")?;
        if let Some(shape) = self.pending {
            match producer.advance_reconfiguration()? {
                ReconfigurationStatus::Ready { .. } => {
                    self.shape = shape;
                    self.pending = None;
                }
                ReconfigurationStatus::PausedCapacity { .. } => return Ok(()),
            }
        }
        let header = &frame.header;
        let shape = (header.width, header.height, header.stride);
        if self.shape != shape {
            match producer.reconfigure_cpu(frame.pixels.len())? {
                ReconfigurationStatus::Ready { .. } => self.shape = shape,
                ReconfigurationStatus::PausedCapacity { .. } => {
                    self.pending = Some(shape);
                    return Ok(());
                }
            }
        }
        self.sequence += 1;
        producer.publish(
            FrameDescriptor {
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
            &frame.pixels,
        )?;
        Ok(())
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        if let Ok(mut producer) = self.producer.lock() {
            producer.stop();
        }
        self.listener.cancel();
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
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
    let mut helper = Helper::spawn(&mut cli.helper_command()?)?;
    println!("{path}");
    eprintln!("luchs: source ready: {path}");
    let watched = cli.local_page().filter(|_| cli.watch);
    let mut modified = watched.as_deref().and_then(modification_time);
    let mut last_poll = Instant::now();
    let mut received = 0;
    while !stop.load(Ordering::Relaxed) {
        match helper.receive(Duration::from_millis(50)) {
            Ok(frame) => {
                source.publish(&frame?)?;
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
    Ok(())
}
