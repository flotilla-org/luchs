//! Optional macOS CEF spike. CEF's browser loop and Jackstay owner share a process.
use crate::{
    Result,
    affordances::{LoadPolicy, PageState},
    capture::Viewport,
    cli::Cli,
    input::CommandPort,
    protocol::{Format, Frame, Header},
    source::Source,
};
use jackstay::{
    acquisition::arena::{
        AcquireOutcome, ArenaConfig, ArenaConsumer, ConfigurationInstall, FrameLease,
        PublishOutcome,
    },
    model::{ColorSpace, PixelFormat},
    native::{
        NativeStreamParams,
        arena::NativeArenaProducer,
        macos::{
            ConsumerFence, IoSurface, MacosCapturedFrame, MacosFrameBackend, SharedEventHandle,
            xpc::arena::XpcArenaServer,
        },
    },
};
use std::{
    ffi::{CStr, CString, c_char, c_void},
    ptr::NonNull,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[repr(C)]
struct Config {
    url: *const c_char,
    cache: *const c_char,
    width: u32,
    height: u32,
    fps: u32,
    gpu: i32,
}
#[repr(C)]
struct Callbacks {
    pixels: unsafe extern "C" fn(*mut c_void, *const u8, u32, u32, u64),
    surface: unsafe extern "C" fn(*mut c_void, *mut c_void, u32, u32, u64),
    state: unsafe extern "C" fn(*mut c_void, *const c_char),
    tick: unsafe extern "C" fn(*mut c_void) -> i32,
    allowed: unsafe extern "C" fn(*mut c_void, *const c_char) -> i32,
}
unsafe extern "C" {
    fn luchs_cef_run(config: *const Config, callbacks: Callbacks, context: *mut c_void) -> i32;
    fn luchs_cef_command(command: *const c_char, timeout_ms: u64) -> i32;
}
fn command(value: serde_json::Value, timeout: Duration) -> jackstay::input::Outcome {
    use jackstay::input::Outcome;
    let Ok(text) = CString::new(value.to_string()) else {
        return Outcome::Unsupported;
    };
    // SAFETY: C++ copies the JSON before returning; the wait is bounded.
    match unsafe {
        luchs_cef_command(
            text.as_ptr(),
            u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        )
    } {
        0 => Outcome::Executed,
        1 => Outcome::Unsupported,
        _ => Outcome::Uncertain,
    }
}
fn note_command(value: serde_json::Value) {
    let kind = value
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_owned();
    let outcome = command(value, Duration::from_secs(1));
    if outcome != jackstay::input::Outcome::Executed {
        eprintln!("luchs cef: {kind} command was not executed: {outcome:?}");
    }
}
struct Gpu {
    producer: Arc<Mutex<NativeArenaProducer<MacosFrameBackend>>>,
    observer: ArenaConsumer,
    fence: ConsumerFence,
    cursor: u64,
    _server: Option<XpcArenaServer>,
    diagnostic: Option<std::path::PathBuf>,
    held: Option<(FrameLease, Vec<u8>)>,
    frames: u64,
    configuration_needed: bool,
}
impl Gpu {
    fn new(width: u32, height: u32) -> Result<Self> {
        let producer = Arc::new(Mutex::new(NativeArenaProducer::new(
            MacosFrameBackend::new()?,
            NativeStreamParams {
                width,
                height,
                pixel_format: PixelFormat::Bgra8Unorm,
                color_space: ColorSpace::Srgb,
                modifier: 0,
            },
            ArenaConfig {
                resource_capacity: 8,
                retained_history: 1,
                producer_reserve: 1,
                payload_capacity: 0,
                memory_budget: 1024 * 1024 * 1024,
                max_incarnations: 4,
                drain_timeout: Duration::from_secs(5),
            },
        )?));
        let (observer, fence) = {
            let mut p = producer.lock().unwrap();
            let grant = p.attach(2)?;
            let fence = ConsumerFence::from_handle(p.backend().metal(), &grant.sync_handle)?;
            (grant.into_consumer()?, fence)
        };
        let server = std::env::var("LUCHS_CEF_MACH_SERVICE")
            .ok()
            .map(|name| XpcArenaServer::start_named(&name, None, producer.clone()))
            .transpose()?;
        Ok(Self {
            producer,
            observer,
            fence,
            cursor: 0,
            _server: server,
            diagnostic: std::env::var_os("LUCHS_CEF_DIAGNOSTICS").map(std::path::PathBuf::from),
            held: None,
            frames: 0,
            configuration_needed: false,
        })
    }
    fn publish(
        &mut self,
        surface: *mut c_void,
        width: u32,
        height: u32,
        stamp: u64,
    ) -> Result<bool> {
        let raw = NonNull::new(surface).ok_or("CEF returned a null IOSurface")?;
        let mut p = self.producer.lock().unwrap();
        if p.reconfiguration_pending() {
            self.observer.relinquish_configuration();
            self.configuration_needed = true;
            p.advance_reconfiguration()?;
        }
        if !p.reconfiguration_pending()
            && (p.params().width != width || p.params().height != height)
        {
            // Reconfigure can install its replacement immediately. Retire our
            // old observer map before either the immediate or capacity-paused
            // transition, then install the offered map before acquiring again.
            self.observer.relinquish_configuration();
            self.configuration_needed = true;
            p.reconfigure(NativeStreamParams {
                width,
                height,
                pixel_format: PixelFormat::Bgra8Unorm,
                color_space: ColorSpace::Srgb,
                modifier: 0,
            })?;
        }
        if p.reconfiguration_pending() {
            self.observer.relinquish_configuration();
            self.configuration_needed = true;
            return Ok(false);
        }
        if self.configuration_needed
            && let Some(grant) = p.configuration_offer(self.observer.incarnation())?
        {
            self.configuration_needed =
                grant.install(&mut self.observer)? != ConfigurationInstall::Installed;
        }
        // SAFETY: CEF lends this IOSurface only for the current callback. Retention
        // preserves the object, not pixel ownership. Wait for our copy before return.
        let captured = MacosCapturedFrame {
            surface: unsafe { IoSurface::from_borrowed(raw) },
        };
        let outcome = p.publish(&captured, stamp)?;
        if let PublishOutcome::Published { cursor } = outcome {
            let AcquireOutcome::Frame(frame) = self.observer.acquire_latest(self.cursor)? else {
                return Err("missing local copy-completion lease".into());
            };
            if frame.descriptor().cursor != cursor {
                return Err("unexpected copy-completion cursor".into());
            }
            // This CPU wait is required by CEF's callback lifetime, not consumer display
            // readiness. Consumers independently GPU-wait on the published timeline.
            if !self.fence.wait(frame.descriptor().fence_value, 1000) {
                eprintln!(
                    "luchs cef: GPU copy did not complete before callback deadline; terminating to avoid returning a borrowed texture still in use"
                );
                std::process::abort();
            }
            self.cursor = cursor;
            self.frames += 1;
            if let Some(directory) = &self.diagnostic {
                if self.frames == 1 || self.frames == 60 {
                    std::fs::create_dir_all(directory)?;
                    let resources = frame.native_resources::<IoSurface, SharedEventHandle>()?;
                    let pixels = self.fence.sample_offscreen(
                        p.backend().metal(),
                        resources.surface,
                        frame.descriptor().fence_value,
                        width,
                        height,
                    )?;
                    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
                    for bgra in pixels.chunks_exact(4) {
                        ppm.extend_from_slice(&[bgra[2], bgra[1], bgra[0]]);
                    }
                    std::fs::write(directory.join(format!("frame-{}.ppm", self.frames)), ppm)?;
                    if self.frames == 1 {
                        self.held = Some((frame, pixels));
                        return Ok(true);
                    }
                    if let Some((old, before)) = self.held.take() {
                        let d = old.descriptor();
                        let resources = old.native_resources::<IoSurface, SharedEventHandle>()?;
                        let after = self.fence.sample_offscreen(
                            p.backend().metal(),
                            resources.surface,
                            d.fence_value,
                            d.width,
                            d.height,
                        )?;
                        if before != after {
                            return Err(
                                "retained Jackstay GPU frame changed while CEF kept rendering"
                                    .into(),
                            );
                        }
                        std::fs::write(
                            directory.join("ownership.json"),
                            serde_json::to_vec_pretty(
                                &serde_json::json!({"retained_frame_stable":true,"later_frame_changed":before!=pixels,"frames_between_samples":59,"initial_cursor":old.cursor(),"later_cursor":frame.cursor(),"initial_pool":d.pool_id,"initial_slot":d.slot_id,"later_slot":frame.descriptor().slot_id,"pixels":[width,height]}),
                            )?,
                        )?;
                    }
                }
            }
            drop(frame);
            return Ok(true);
        }
        Ok(false)
    }
}
struct Context {
    source: Mutex<Source>,
    page: PageState,
    policy: LoadPolicy,
    stop: Arc<AtomicBool>,
    gpu: Mutex<Option<Gpu>>,
    gpu_enabled: bool,
    count: Mutex<u64>,
    limit: u32,
    failed: Mutex<Option<String>>,
    scale: Mutex<f64>,
    logical_size: (u32, u32),
    started: Instant,
    trace: Mutex<Option<std::fs::File>>,
}
impl Context {
    fn fail(&self, error: impl ToString) {
        *self.failed.lock().unwrap() = Some(error.to_string());
        self.stop.store(true, Ordering::Release);
    }
    fn published(&self) {
        *self.count.lock().unwrap() += 1;
    }
}
unsafe fn context<'a>(raw: *mut c_void) -> &'a Context {
    unsafe { &*raw.cast::<Context>() }
}
unsafe extern "C" fn pixels(
    raw: *mut c_void,
    bytes: *const u8,
    width: u32,
    height: u32,
    stamp: u64,
) {
    let ctx = unsafe { context(raw) };
    if ctx.gpu_enabled {
        return;
    }
    if let Err(e) = crate::protocol::validate_size(width, height) {
        ctx.fail(e);
        return;
    }
    let len = width as usize * height as usize * 4;
    // SAFETY: OnPaint lends width*height*4 packed BGRA bytes until callback return.
    let pixels = unsafe { std::slice::from_raw_parts(bytes, len) }.to_vec();
    let result = ctx.source.lock().unwrap().publish_at(
        Frame {
            header: Header {
                format: Format::Bgra8,
                width,
                height,
                stride: width * 4,
                len,
            },
            pixels,
        },
        stamp,
    );
    match result {
        Ok(true) => ctx.published(),
        Ok(false) => {}
        Err(e) => ctx.fail(e),
    }
}
unsafe extern "C" fn surface(
    raw: *mut c_void,
    surface: *mut c_void,
    width: u32,
    height: u32,
    stamp: u64,
) {
    let ctx = unsafe { context(raw) };
    let began = Instant::now();
    let mut gpu = ctx.gpu.lock().unwrap();
    if gpu.is_none() {
        match Gpu::new(width, height) {
            Ok(value) => *gpu = Some(value),
            Err(e) => {
                ctx.fail(e);
                return;
            }
        }
    }
    match gpu.as_mut().unwrap().publish(surface, width, height, stamp) {
        Ok(true) => {
            ctx.published();
            ctx.page.frame_published();
            if gpu.as_ref().unwrap().frames <= 600 {
                if let Some(file) = ctx.trace.lock().unwrap().as_mut() {
                    use std::io::Write;
                    let _ = writeln!(
                        file,
                        "{},{},{},{},{}",
                        stamp,
                        width,
                        height,
                        began.elapsed().as_nanos(),
                        gpu.as_ref().unwrap().frames
                    );
                }
            }
        }
        Ok(false) => {}
        Err(e) => ctx.fail(e),
    }
}
unsafe extern "C" fn state(raw: *mut c_void, json: *const c_char) {
    let ctx = unsafe { context(raw) };
    // SAFETY: C++ lends a NUL-terminated JSON state for this callback.
    if let Ok(value) = serde_json::from_slice(unsafe { CStr::from_ptr(json) }.to_bytes()) {
        ctx.page.helper_state(value);
    }
}
unsafe extern "C" fn allowed(raw: *mut c_void, url: *const c_char) -> i32 {
    let ctx = unsafe { context(raw) };
    // SAFETY: C++ lends a NUL-terminated URL for this callback.
    unsafe { CStr::from_ptr(url) }
        .to_str()
        .ok()
        .and_then(|url| ctx.policy.allowed_url(url))
        .is_some() as i32
}
unsafe extern "C" fn tick(raw: *mut c_void) -> i32 {
    let ctx = unsafe { context(raw) };
    if ctx.stop.load(Ordering::Acquire)
        || ctx.limit > 0 && *ctx.count.lock().unwrap() >= u64::from(ctx.limit)
    {
        return 1;
    }
    let presentation = { ctx.source.lock().unwrap().presentation() };
    if let Some(hint) = presentation {
        let viewport = {
            let mut scale = ctx.scale.lock().unwrap();
            let viewport = Viewport::resolve(ctx.logical_size, &hint, *scale);
            *scale = viewport.scale;
            viewport
        };
        let width = viewport.logical.0;
        let height = viewport.logical.1;
        let outcome = command(
            serde_json::json!({"type":"resize","width":width,"height":height,"scale":viewport.scale}),
            Duration::from_secs(1),
        );
        if outcome != jackstay::input::Outcome::Executed {
            ctx.fail("CEF resize failed");
            return 1;
        }
        ctx.source.lock().unwrap().set_logical_size(width, height);
        note_command(serde_json::json!({"type":"presentation","visible":hint.visible}));
        note_command(serde_json::json!({"type":"focus","focused":hint.focused}));
    }
    if let Some(value) = ctx.page.command() {
        note_command(value);
    }
    0
}
pub fn run(cli: Cli, stop: Arc<AtomicBool>) -> Result<()> {
    if cli.watch {
        return Err("the CEF spike does not support --watch yet".into());
    }
    let local = cli.local_page();
    let policy = LoadPolicy::new(local.as_deref())?;
    let url = match &local {
        Some(path) => url::Url::from_file_path(path.canonicalize()?)
            .map_err(|_| "invalid file URL")?
            .to_string(),
        None => cli.page.clone(),
    };
    let name = cli
        .endpoint
        .clone()
        .unwrap_or_else(|| format!("luchs-cef-{}", std::process::id()));
    let (source, path) = Source::bind_page(&name, cli.size.0, cli.size.1, local.as_deref())?;
    source.attach_engine(CommandPort::new(command));
    let page = source.page.clone();
    let cache = tempfile::Builder::new().prefix("luchs-cef-").tempdir()?;
    let url = CString::new(url)?;
    let cache_name = CString::new(cache.path().to_str().ok_or("non-UTF8 cache path")?)?;
    let gpu_enabled = std::env::var_os("LUCHS_CEF_GPU").is_some();
    let ctx = Context {
        source: Mutex::new(source),
        page,
        policy,
        stop,
        gpu: Mutex::new(None),
        gpu_enabled,
        count: Mutex::new(0),
        limit: cli.frames,
        failed: Mutex::new(None),
        scale: Mutex::new(1.),
        logical_size: (cli.size.0, cli.size.1),
        started: Instant::now(),
        trace: Mutex::new(
            std::env::var_os("LUCHS_CEF_TRACE")
                .map(std::fs::File::create)
                .transpose()?,
        ),
    };
    let config = Config {
        url: url.as_ptr(),
        cache: cache_name.as_ptr(),
        width: cli.size.0,
        height: cli.size.1,
        fps: cli.fps,
        gpu: gpu_enabled as i32,
    };
    println!("{path}");
    eprintln!("luchs: source ready: {path}");
    // SAFETY: CEF blocks on this thread until browsers close. All callbacks finish
    // before return, and ctx plus configuration strings remain alive throughout.
    let status = unsafe {
        luchs_cef_run(
            &config,
            Callbacks {
                pixels,
                surface,
                state,
                tick,
                allowed,
            },
            (&ctx as *const Context).cast_mut().cast(),
        )
    };
    let count = *ctx.count.lock().unwrap();
    eprintln!(
        "luchs: cef {} frames={count} elapsed_ms={:.3}",
        if gpu_enabled { "gpu" } else { "cpu" },
        ctx.started.elapsed().as_secs_f64() * 1000.
    );
    let failed = ctx.failed.lock().unwrap().take();
    drop(ctx);
    drop(cache);
    if let Some(error) = failed {
        return Err(error.into());
    }
    if status != 0 {
        return Err(format!("CEF exited with status {status}").into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_observer_follows_immediately_installed_resize() {
        let mut gpu = Gpu::new(8, 8).unwrap();
        for (stamp, (width, height)) in [(8, 8), (12, 10), (8, 8)].into_iter().enumerate() {
            let surface = IoSurface::allocate(width, height, PixelFormat::Bgra8Unorm).unwrap();
            surface
                .write_pixels(&vec![127; (width * height * 4) as usize])
                .unwrap();
            assert!(
                gpu.publish(surface.as_raw(), width, height, stamp as u64 + 1)
                    .unwrap()
            );
        }
        assert_eq!(gpu.frames, 3);
    }
}
