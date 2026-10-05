//! Helper frames and native input callbacks for the native producer toolkit.
use crate::{
    Result,
    affordances::{LoadPolicy, PageState},
    capture::{CapturePolicy, PRESENTATION_DEBOUNCE, Viewport},
    cli::Cli,
    helper::{CommandOutcome, Helper, HelperEvent},
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
    fs,
    os::unix::fs::PermissionsExt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

struct PageProducer {
    input: crate::input::Executor,
    page: PageState,
    load_policy: LoadPolicy,
    presentation: Arc<Mutex<Option<jackstay::affordances::Presentation>>>,
    wake: Arc<Mutex<Option<crate::helper::Wake>>>,
    logical_size: Arc<Mutex<(f64, f64)>>,
    input_sender: Arc<Mutex<Option<crate::helper::CommandSender>>>,
}
impl Producer for PageProducer {
    fn frame(&mut self) -> Option<jackstay_producer::Frame> {
        None
    }

    fn execute(&mut self, work: Work) -> Outcome {
        // The toolkit owns and serializes the executor. Only attachment is
        // shared; release its lock before the helper acknowledgement wait.
        let sender = self.input_sender.lock().unwrap().clone();
        if let Some(sender) = sender {
            self.input.attach(sender);
        }
        self.input
            .set_viewport_height(self.logical_size.lock().unwrap().1);
        self.input.execute(work)
    }
    fn snapshots(&mut self) -> Vec<jackstay::affordances::Snapshot> {
        self.page.snapshots()
    }
    fn input_size(&mut self, _width: u32, _height: u32) -> (f64, f64) {
        *self.logical_size.lock().unwrap()
    }
    fn input_geometry(&mut self) -> Option<(f64, f64)> {
        Some(*self.logical_size.lock().unwrap())
    }
    fn affordance(&mut self, event: jackstay::affordances::Event) {
        use jackstay::affordances::{Domain, Event, Presentation, Snapshot};
        if let Event::Verb(verb) = event {
            self.page.verb(verb, &self.load_policy);
            if let Some(wake) = self.wake.lock().unwrap().as_ref() {
                wake.notify();
            }
            return;
        }
        let hint = match event {
            Event::Snapshot(Snapshot::Presentation(hint)) => hint,
            Event::Snapshot(Snapshot::Withdraw(Domain::Presentation)) | Event::Closed => {
                Presentation::default()
            }
            _ => return,
        };
        *self.presentation.lock().unwrap() = Some(hint);
        if let Some(wake) = self.wake.lock().unwrap().as_ref() {
            wake.notify();
        }
    }
}

pub struct Source {
    pub page: PageState,
    wake: Arc<Mutex<Option<crate::helper::Wake>>>,
    presentation: Arc<Mutex<Option<jackstay::affordances::Presentation>>>,
    source: jackstay_producer::Source,
    input_sender: Arc<Mutex<Option<crate::helper::CommandSender>>>,
    started: Instant,
    sequence: u64,
    mapping: Option<crate::arena_capture::Mapping>,
    logical_size: Arc<Mutex<(f64, f64)>>,
    dimensions: (u32, u32, u32),
    pending_dimensions: Option<(u32, u32, u32)>,
}
impl Source {
    pub fn bind(name: &str, width: u32, height: u32) -> Result<(Self, String)> {
        Self::bind_page(name, width, height, None)
    }

    pub fn bind_page(
        name: &str,
        width: u32,
        height: u32,
        page_path: Option<&std::path::Path>,
    ) -> Result<(Self, String)> {
        Self::bind_page_with_budget(name, width, height, page_path, 1024 * 1024 * 1024)
    }

    fn bind_page_with_budget(
        name: &str,
        width: u32,
        height: u32,
        page_path: Option<&std::path::Path>,
        memory_budget: u64,
    ) -> Result<(Self, String)> {
        let load_policy = LoadPolicy::new(page_path)?;
        let page = PageState::default();
        crate::protocol::validate_size(width, height)?;
        let endpoint = Endpoint::new(Scope::User, name, Transport::LocalStream)?;
        let path = endpoint.render()?;
        let presentation = Arc::new(Mutex::new(None));
        let wake = Arc::new(Mutex::new(None));
        let input_sender = Arc::new(Mutex::new(None));
        let logical_size = Arc::new(Mutex::new((f64::from(width), f64::from(height))));
        let source = Builder::new(
            endpoint,
            ArenaConfig {
                resource_capacity: 8,
                retained_history: 1,
                producer_reserve: 1,
                payload_capacity: width as usize * height as usize * 4,
                // Frames are capped at 64 MiB; 1 GiB bounds aggregate arena allocations.
                memory_budget,
                max_incarnations: 3,
                drain_timeout: Duration::from_secs(5),
            },
            Config {
                modes: 7,
                capabilities: jackstay::input::CAP_ALL,
                geometry: jackstay::input::Geometry {
                    revision: 1,
                    width: width.into(),
                    height: height.into(),
                },
                ..Config::default()
            },
            PageProducer {
                page: page.clone(),
                load_policy,
                presentation: presentation.clone(),
                wake: wake.clone(),
                logical_size: logical_size.clone(),
                input: crate::input::Executor::new(f64::from(height)),
                input_sender: input_sender.clone(),
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
                page,
                wake,
                input_sender,
                presentation,
                source,
                started: Instant::now(),
                sequence: 0,
                mapping: None,
                logical_size,
                dimensions: (width, height, width * 4),
                pending_dimensions: None,
            },
            path,
        ))
    }

    pub fn attach_input(&self, helper: &Helper) {
        *self.input_sender.lock().unwrap() = Some(helper.command_sender());
    }

    fn setup_writer(&mut self, helper: &Helper) -> Result<()> {
        self.mapping.take(); // reap a replaced helper before retiring its export
        let export = self
            .source
            .with_arena(|arena| arena.export_writer())?
            .ok_or("arena paused during setup")?;
        self.mapping = Some(crate::arena_capture::Mapping::install(helper, export)?);
        Ok(())
    }

    /// Publish locally supplied pixels, for non-helper producers and fixtures.
    /// Helper draws use `draw`/`complete_draw` and never enter this copying path.
    pub fn publish(&mut self, frame: Frame) -> Result<()> {
        if self.source.is_finished() {
            return Err("source pump stopped".into());
        }
        crate::protocol::validate_size(frame.header.width, frame.header.height)?;
        if frame.header.stride < frame.header.width * 4
            || frame.header.len != frame.pixels.len()
            || frame.pixels.len() != frame.header.stride as usize * frame.header.height as usize
            || frame.pixels.len() > crate::protocol::MAX_FRAME_BYTES
        {
            return Err("invalid local frame".into());
        }
        let dimensions = (frame.header.width, frame.header.height, frame.header.stride);
        if !self.configure_dimensions(dimensions, frame.pixels.len())? {
            return Ok(());
        }
        self.sequence += 1;
        self.source.with_arena(|arena| -> Result<()> {
            if let Some(mut slot) = arena.reserve()? {
                slot.bytes_mut()[..frame.pixels.len()].copy_from_slice(&frame.pixels);
                arena.commit(
                    slot,
                    FrameDescriptor {
                        sequence: self.sequence,
                        timestamp_ns: self.started.elapsed().as_nanos() as u64,
                        width: frame.header.width,
                        height: frame.header.height,
                        stride: frame.header.stride,
                        payload_len: frame.pixels.len() as u64,
                        pixel_format: PixelFormat::Bgra8Unorm as u32,
                        clock_domain: ClockDomain::MediaTime as u32,
                        sync_kind: FrameSyncKind::CpuCopyComplete as u32,
                        damage_kind: DamageKind::FullFrame as u32,
                        ..FrameDescriptor::default()
                    },
                )?;
                self.page.frame_published();
            }
            Ok(())
        })
    }

    pub fn draw(
        &mut self,
        helper: &Helper,
        width: u32,
        height: u32,
        timeout: Duration,
    ) -> Result<Option<crate::arena_capture::Draw>> {
        crate::protocol::validate_size(width, height)?;
        let dimensions = (width, height, width * 4);
        let len = width as usize * height as usize * 4;
        if !self.configure_dimensions(dimensions, len)? {
            return Ok(None);
        }
        if self.mapping.is_none() {
            let Some(export) = self.source.with_arena(|arena| arena.export_writer())? else {
                return Ok(None);
            };
            self.mapping = Some(crate::arena_capture::Mapping::install(helper, export)?);
        }
        let Some(reservation) = self.source.with_arena(|arena| arena.reserve())? else {
            return Ok(None);
        };
        let header = crate::protocol::Header {
            format: crate::protocol::Format::Bgra8,
            width,
            height,
            stride: width * 4,
            len,
        };
        Ok(Some(crate::arena_capture::Draw::start(
            helper,
            reservation,
            header,
            timeout,
        )?))
    }

    fn configure_dimensions(&mut self, requested: (u32, u32, u32), len: usize) -> Result<bool> {
        use jackstay::acquisition::arena::ReconfigurationStatus;
        if self.pending_dimensions.is_none() && self.dimensions == requested {
            return Ok(true);
        }
        let status = if self.pending_dimensions.is_some() {
            self.source
                .with_arena(|arena| arena.advance_reconfiguration())?
        } else {
            if let Some(mapping) = self.mapping.take() {
                mapping.release()?;
            }
            self.pending_dimensions = Some(requested);
            self.source.with_arena(|arena| arena.reconfigure_cpu(len))?
        };
        if matches!(status, ReconfigurationStatus::Ready { .. }) {
            self.dimensions = self.pending_dimensions.take().unwrap();
            return Ok(self.dimensions == requested);
        }
        Ok(false)
    }

    pub fn complete_draw(
        &mut self,
        helper: &Helper,
        draw: crate::arena_capture::Draw,
        ack: crate::protocol::Ack,
    ) -> Result<Option<crate::protocol::CaptureReport>> {
        self.finish_draw(helper, draw, ack, true)
    }

    fn finish_draw(
        &mut self,
        helper: &Helper,
        mut draw: crate::arena_capture::Draw,
        mut ack: crate::protocol::Ack,
        visible: bool,
    ) -> Result<Option<crate::protocol::CaptureReport>> {
        if !helper.running()? {
            return Err("renderer died during draw".into());
        }
        draw.validate(&ack)?;
        let reservation = draw.reservation.complete();
        if !visible {
            if let Some(report) = ack.capture.as_mut() {
                report.published = false;
                report.publish_ns = 0;
            }
        }
        if ack.capture.as_ref().is_some_and(|r| r.published) {
            self.sequence += 1;
            self.source.with_arena(|arena| {
                arena.commit(
                    reservation,
                    FrameDescriptor {
                        sequence: self.sequence,
                        timestamp_ns: self.started.elapsed().as_nanos() as u64,
                        width: draw.header.width,
                        height: draw.header.height,
                        stride: draw.header.stride,
                        payload_len: draw.header.len as u64,
                        pixel_format: PixelFormat::Bgra8Unorm as u32,
                        clock_domain: ClockDomain::MediaTime as u32,
                        sync_kind: FrameSyncKind::CpuCopyComplete as u32,
                        damage_kind: DamageKind::FullFrame as u32,
                        ..FrameDescriptor::default()
                    },
                )
            })?;
            self.page.frame_published();
        } else {
            self.source.with_arena(|arena| arena.abandon(reservation))?;
        }
        Ok(ack.capture)
    }

    pub fn helper_stopped(&self) {
        self.page.helper_stopped();
    }

    pub fn presentation(&self) -> Option<jackstay::affordances::Presentation> {
        self.presentation.lock().unwrap().take()
    }

    pub fn stop(mut self) -> Result<()> {
        // Unmap pixels without terminating the helper: native input cleanup still
        // needs its command endpoint while the toolkit stops.
        if let Some(mapping) = self.mapping.take() {
            mapping.release()?;
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
    fn report(
        &mut self,
        report: Option<crate::protocol::CaptureReport>,
        policy: &mut CapturePolicy,
    ) {
        if let Some(report) = report {
            self.snapshots += 1;
            self.skipped += u64::from(!report.published);
            self.snapshot_ns += u128::from(report.snapshot_ns);
            self.publish_ns += u128::from(report.publish_ns);
            policy.completed(report.published, Instant::now());
        } else {
            policy.completed(true, Instant::now());
        }
    }
    fn log(&self, received: u64) {
        // One explicit destination write is a path invariant, not a runtime
        // hardware counter; kernel/WebKit internals and hashing reads are excluded.
        eprintln!(
            "luchs: stats snapshots={} published={received} skipped={} copies_per_frame=1 mean_snapshot_ms={:.3} mean_publish_ms={:.3}",
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
    let (mut source, path) =
        Source::bind_page(&name, cli.size.0, cli.size.1, cli.local_page().as_deref())?;
    // Capture the baseline before exposing readiness or starting the helper;
    // an edit after startup must not become the baseline and miss its reload.
    let watched = cli.local_page().filter(|_| cli.watch);
    let mut modified = watched.as_deref().and_then(modification_time);
    let changed = Arc::new(AtomicBool::new(false));
    let activity = changed.clone();
    let page = source.page.clone();
    let mut helper = Helper::spawn_with_state(&mut cli.helper_command()?, move |state| {
        if state
            .get("capture_changed")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            activity.store(true, Ordering::Release);
        }
        page.helper_state(state);
    })?;
    source.attach_input(&helper);
    source.setup_writer(&helper)?;
    *source.wake.lock().unwrap() = Some(helper.wake_handle());
    let mut policy = CapturePolicy::new(cli.fps, Instant::now());
    let mut viewport = Viewport {
        logical: (cli.size.0, cli.size.1),
        pixels: (cli.size.0, cli.size.1),
        scale: 1.,
    };
    let mut focused = false;
    let mut pending_presentation = None;
    let mut pending_capture: Option<crate::arena_capture::Draw> = None;
    let mut stats = CaptureStats::default();
    println!("{path}");
    eprintln!("luchs: source ready: {path}");
    let mut last_poll = Instant::now();
    let mut received = 0;
    let mut last_reload_failure = None;
    let mut command_socket_closed = false;
    let mut pending_verb: Option<crate::helper::PendingCommand> = None;
    while !stop.load(Ordering::Relaxed) {
        // Acks, page activity and presentation callbacks interrupt this wait.
        // The 250 ms ceiling services file-watch and signal flags even hidden.
        let watch_wait = Duration::from_millis(250).saturating_sub(last_poll.elapsed());
        let timeout = if let Some(command) = pending_capture.as_ref() {
            command.remaining().min(watch_wait)
        } else if command_socket_closed {
            watch_wait
        } else {
            policy.wait(Instant::now()).min(watch_wait)
        };
        let timeout =
            pending_presentation
                .as_ref()
                .map_or(timeout, |(_, deadline): &(_, Instant)| {
                    if pending_capture.is_none() {
                        timeout.min(deadline.saturating_duration_since(Instant::now()))
                    } else {
                        timeout
                    }
                });
        match helper.receive_event(timeout) {
            Ok(HelperEvent::Wake | HelperEvent::Timeout) => {}
            Err(error) => return Err(error.into()),
            Ok(HelperEvent::Closed) => {
                source.page.helper_stopped();
                helper.finish()?;
                pending_capture.take();
                if received == 0 {
                    return Err("renderer exited without a frame".into());
                }
                break;
            }
        }
        if changed.swap(false, Ordering::AcqRel) {
            policy.wake(Instant::now());
        }
        if let Some(hint) = source.presentation() {
            pending_presentation = Some((hint, Instant::now() + PRESENTATION_DEBOUNCE));
        }
        if pending_verb
            .as_ref()
            .is_some_and(|p| p.poll().is_some() || p.expired())
        {
            pending_verb = None;
        }
        if pending_verb.is_none() && !command_socket_closed && !helper.ended() {
            if let Some(command) = source.page.command() {
                pending_verb =
                    Some(helper.send_json_command(command, crate::helper::COMMAND_TIMEOUT)?);
                policy.wake(Instant::now());
            }
        }
        if let Some(draw) = pending_capture.as_ref() {
            if let Some(ack) = draw.poll() {
                let report = source.finish_draw(
                    &helper,
                    pending_capture.take().unwrap(),
                    ack,
                    policy.visible,
                )?;
                let published = report.as_ref().is_some_and(|r| r.published);
                stats.report(report, &mut policy);
                received += u64::from(published);
                if cli.frames > 0 && received >= u64::from(cli.frames) {
                    break;
                }
            } else if draw.expired() {
                return Err("renderer capture timed out".into());
            }
        }
        if pending_capture.is_none()
            && pending_presentation
                .as_ref()
                .is_some_and(|(_, deadline)| Instant::now() >= *deadline)
        {
            let (hint, _) = pending_presentation.take().unwrap();
            let next = Viewport::resolve((cli.size.0, cli.size.1), &hint, viewport.scale);
            let mut commands = Vec::new();
            if next != viewport {
                commands.push(serde_json::json!({"type":"resize", "width":next.logical.0, "height":next.logical.1, "scale":next.scale}));
            }
            if hint.focused != focused {
                commands.push(serde_json::json!({"type":"focus", "focused":hint.focused}));
            }
            commands.push(serde_json::json!({"type":"presentation", "visible":hint.visible, "scale":next.scale}));
            for command in commands {
                let kind = command["type"].as_str().unwrap().to_owned();
                let outcome = helper
                    .send_json_command(command, crate::helper::COMMAND_TIMEOUT)?
                    .wait();
                if outcome != CommandOutcome::Executed {
                    return Err(format!("renderer {kind}: {outcome:?}").into());
                }
            }
            viewport = next;
            focused = hint.focused;
            *source.logical_size.lock().unwrap() =
                (f64::from(viewport.logical.0), f64::from(viewport.logical.1));
            policy.presentation(hint.visible, viewport.scale, Instant::now());
        }
        if pending_capture.is_none()
            && !command_socket_closed
            && !helper.ended()
            && policy.due(Instant::now())
        {
            match source.draw(
                &helper,
                viewport.pixels.0,
                viewport.pixels.1,
                Duration::from_secs(10),
            ) {
                Ok(command) => {
                    pending_capture = command;
                    if pending_capture.is_none() {
                        policy.completed(false, Instant::now());
                    }
                }
                Err(error)
                    if matches!(
                        error
                            .downcast_ref::<std::io::Error>()
                            .map(std::io::Error::kind),
                        Some(std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset)
                    ) =>
                {
                    command_socket_closed = true;
                }
                Err(error) => return Err(error),
            }
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
    // Complete an in-flight draw before ordered input cleanup. Reaping a
    // cancelled writer first would also remove the native cleanup endpoint.
    if let Some(draw) = pending_capture.take() {
        let deadline = Instant::now() + draw.remaining().min(Duration::from_secs(1));
        loop {
            if let Some(ack) = draw.poll() {
                let report = source.finish_draw(&helper, draw, ack, policy.visible)?;
                let published = report.as_ref().is_some_and(|r| r.published);
                stats.report(report, &mut policy);
                received += u64::from(published);
                break;
            }
            if helper.ended() || Instant::now() >= deadline {
                return Err("renderer stopped or timed out during final draw".into());
            }
            helper.receive_event(deadline.saturating_duration_since(Instant::now()))?;
        }
    }
    eprintln!("luchs: stopped after {received} frames");
    if cli.stats {
        stats.log(received);
    }
    let ignored = helper.ignored_acks();
    if ignored > 0 {
        eprintln!("luchs: helper ignored {ignored} unmatched or late acks");
    }
    source.stop()?;
    Ok(())
}

#[cfg(test)]
#[path = "arena_capture_tests.rs"]
mod arena_capture_tests;
