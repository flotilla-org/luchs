// Throwaway WebView2 offscreen-capture spike (modelled on GStreamer webview2src).
#![allow(clippy::all)]
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::*;
use windows::core::{Interface, HSTRING, PCWSTR, PWSTR};
use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Graphics::Capture::*;
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::UI::Composition::{Compositor, ContainerVisual};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;
use windows::Win32::System::Com::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::Direct3D11::*;
use windows::Win32::System::WinRT::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows_numerics::Vector2;

macro_rules! log { ($($t:tt)*) => {{ println!($($t)*); }} }

struct D3D {
    dev: ID3D11Device,
    ctx: ID3D11DeviceContext,
}
unsafe impl Send for D3D {}
unsafe impl Sync for D3D {}

struct Shared {
    count: AtomicU64,
    times: Mutex<Vec<Instant>>,
    last: Mutex<(i32, i32, u32, u32)>, // content w,h ; surface w,h
    save: Mutex<Option<String>>,
    saved: Mutex<Option<String>>,
    d3d: D3D,
}

fn pump(ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    loop {
        unsafe {
            let mut m = MSG::default();
            while PeekMessageW(&mut m, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&m);
                DispatchMessageW(&m);
            }
        }
        let now = Instant::now();
        if now >= end {
            break;
        }
        let rem = (end - now).as_millis() as u32;
        unsafe {
            MsgWaitForMultipleObjects(None, false, rem.min(20), QS_ALLINPUT);
        }
    }
}

fn exec(wv: &ICoreWebView2, js: &str) -> String {
    let out = Rc::new(RefCell::new(String::new()));
    let o2 = out.clone();
    let wv2 = wv.clone();
    let js = js.to_string();
    let r = ExecuteScriptCompletedHandler::wait_for_async_operation(
        Box::new(move |h| unsafe {
            let js = CoTaskMemPWSTR::from(js.as_str());
            wv2.ExecuteScript(*js.as_ref().as_pcwstr(), &h)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |hr, res| {
            *o2.borrow_mut() = match hr {
                Ok(()) => res,
                Err(e) => format!("ERR {e:?}"),
            };
            Ok(())
        }),
    );
    if let Err(e) = r {
        return format!("ERR {e:?}");
    }
    out.take()
}

fn cdp(wv: &ICoreWebView2, method: &str, params: &str) -> String {
    let out = Rc::new(RefCell::new(String::new()));
    let o2 = out.clone();
    let wv2 = wv.clone();
    let m = method.to_string();
    let p = params.to_string();
    let r = CallDevToolsProtocolMethodCompletedHandler::wait_for_async_operation(
        Box::new(move |h| unsafe {
            let m = CoTaskMemPWSTR::from(m.as_str());
            let p = CoTaskMemPWSTR::from(p.as_str());
            wv2.CallDevToolsProtocolMethod(*m.as_ref().as_pcwstr(), *p.as_ref().as_pcwstr(), &h)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |hr, res| {
            *o2.borrow_mut() = match hr {
                Ok(()) => res,
                Err(e) => format!("ERR {e:?} {res}"),
            };
            Ok(())
        }),
    );
    if let Err(e) = r {
        return format!("ERR {e:?}");
    }
    out.take()
}

fn fg() -> String {
    unsafe {
        let h = GetForegroundWindow();
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, Some(&mut pid));
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(h, &mut buf);
        format!(
            "hwnd={:?} pid={} title={:?}",
            h.0,
            pid,
            String::from_utf16_lossy(&buf[..n as usize])
        )
    }
}

fn save_frame(d3d: &D3D, frame: &Direct3D11CaptureFrame, path: &str) -> windows::core::Result<String> {
    unsafe {
        let surf = frame.Surface()?;
        let access: IDirect3DDxgiInterfaceAccess = surf.cast()?;
        let tex: ID3D11Texture2D = access.GetInterface()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        tex.GetDesc(&mut desc);
        let mut sd = desc;
        sd.Usage = D3D11_USAGE_STAGING;
        sd.BindFlags = 0;
        sd.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
        sd.MiscFlags = 0;
        sd.MipLevels = 1;
        sd.ArraySize = 1;
        let mut staging: Option<ID3D11Texture2D> = None;
        d3d.dev.CreateTexture2D(&sd, None, Some(&mut staging))?;
        let staging = staging.unwrap();
        d3d.ctx.CopyResource(&staging, &tex);
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        d3d.ctx.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        let (w, h) = (desc.Width as usize, desc.Height as usize);
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            let src = std::slice::from_raw_parts(
                (mapped.pData as *const u8).add(y * mapped.RowPitch as usize),
                w * 4,
            );
            for x in 0..w {
                let s = &src[x * 4..x * 4 + 4];
                let d = &mut rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
                d[0] = s[2];
                d[1] = s[1];
                d[2] = s[0];
                d[3] = s[3];
            }
        }
        d3d.ctx.Unmap(&staging, 0);
        // stats
        let px = |x: usize, y: usize| {
            let i = (y * w + x) * 4;
            format!("{:02x}{:02x}{:02x}{:02x}", rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3])
        };
        let nonwhite = rgba.chunks(4).filter(|p| !(p[0] > 250 && p[1] > 250 && p[2] > 250)).count();
        let yellowish = rgba.chunks(4).filter(|p| p[0] > 200 && p[1] > 150 && p[2] < 80).count();
        let f = std::fs::File::create(path).unwrap();
        let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut wr = enc.write_header().unwrap();
        wr.write_image_data(&rgba).unwrap();
        Ok(format!(
            "saved {path} tex={}x{} fmt={:?} px(0,0)={} px(1,h/2)={} px(w/2,h/2)={} px(w-1,h-1)={} nonwhite={} yellowish={}",
            w, h, desc.Format, px(0, 0), px(1, h / 2), px(w / 2, h / 2), px(w - 1, h - 1), nonwhite, yellowish
        ))
    }
}

struct Cap {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
}

fn start_capture(
    item: &GraphicsCaptureItem,
    wdev: &IDirect3DDevice,
    shared: &Arc<Shared>,
    w: i32,
    h: i32,
    border: Option<bool>,
    cursor: Option<bool>,
    min_ms: Option<i64>,
) -> windows::core::Result<Cap> {
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        wdev,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        SizeInt32 { Width: w, Height: h },
    )?;
    let sh = shared.clone();
    pool.FrameArrived(&TypedEventHandler::new(
        move |p: windows::core::Ref<Direct3D11CaptureFramePool>, _| {
            let p = p.ok()?;
            let frame = p.TryGetNextFrame()?;
            sh.count.fetch_add(1, Ordering::SeqCst);
            sh.times.lock().unwrap().push(Instant::now());
            let cs = frame.ContentSize()?;
            let mut sw = 0;
            let mut shh = 0;
            if let Ok(surf) = frame.Surface() {
                if let Ok(d) = surf.Description() {
                    sw = d.Width as u32;
                    shh = d.Height as u32;
                }
            }
            *sh.last.lock().unwrap() = (cs.Width, cs.Height, sw, shh);
            let req = sh.save.lock().unwrap().take();
            if let Some(path) = req {
                let r = match save_frame(&sh.d3d, &frame, &path) {
                    Ok(s) => s,
                    Err(e) => format!("save error {e:?}"),
                };
                *sh.saved.lock().unwrap() = Some(format!("{r} contentSize={}x{}", cs.Width, cs.Height));
            }
            frame.Close()?;
            Ok(())
        },
    ))?;
    let session = pool.CreateCaptureSession(item)?;
    if let Some(b) = border {
        log!("  SetIsBorderRequired({b}) -> {:?}", session.SetIsBorderRequired(b));
    }
    if let Some(c) = cursor {
        log!("  SetIsCursorCaptureEnabled({c}) -> {:?}", session.SetIsCursorCaptureEnabled(c));
    }
    if let Some(ms) = min_ms {
        log!(
            "  SetMinUpdateInterval({ms}ms) -> {:?}",
            session.SetMinUpdateInterval(TimeSpan { Duration: ms * 10_000 })
        );
    }
    log!(
        "  session props: IsBorderRequired={:?} IsCursorCaptureEnabled={:?} MinUpdateInterval={:?}",
        session.IsBorderRequired(),
        session.IsCursorCaptureEnabled(),
        session.MinUpdateInterval().map(|t| t.Duration as f64 / 10_000.0)
    );
    session.StartCapture()?;
    Ok(Cap { pool, session })
}

fn stop_capture(c: Cap) {
    let _ = c.session.Close();
    let _ = c.pool.Close();
}

fn measure(shared: &Arc<Shared>, wv: &ICoreWebView2, ms: u64, label: &str) -> (f64, f64) {
    let raf0: f64 = exec(wv, "raf").parse().unwrap_or(-1.0);
    let c0 = shared.count.load(Ordering::SeqCst);
    shared.times.lock().unwrap().clear();
    let t0 = Instant::now();
    pump(ms);
    let dt = t0.elapsed().as_secs_f64();
    let c1 = shared.count.load(Ordering::SeqCst);
    let raf1: f64 = exec(wv, "raf").parse().unwrap_or(-1.0);
    let times = shared.times.lock().unwrap().clone();
    let mut gaps: Vec<f64> = times.windows(2).map(|w| (w[1] - w[0]).as_secs_f64() * 1000.0).collect();
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let (mn, med, mx) = if gaps.is_empty() {
        (0.0, 0.0, 0.0)
    } else {
        (gaps[0], gaps[gaps.len() / 2], gaps[gaps.len() - 1])
    };
    let fps = (c1 - c0) as f64 / dt;
    let rafps = (raf1 - raf0) / dt;
    log!(
        "[{label}] {:.1}s: frames={} => {:.1} fps; gap ms min/med/max={:.1}/{:.1}/{:.1}; rAF {:.1}/s; last={:?}; fg: {}",
        dt,
        c1 - c0,
        fps,
        mn,
        med,
        mx,
        rafps,
        *shared.last.lock().unwrap(),
        fg()
    );
    (fps, rafps)
}

fn request_save(shared: &Arc<Shared>, path: &str) -> String {
    *shared.saved.lock().unwrap() = None;
    *shared.save.lock().unwrap() = Some(path.to_string());
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(5) {
        pump(50);
        if let Some(s) = shared.saved.lock().unwrap().take() {
            return s;
        }
    }
    *shared.save.lock().unwrap() = None;
    "NO FRAME within 5s".into()
}

const PAGE: &str = r##"<!doctype html><html><head><title>spike</title></head>
<body style="margin:0;background:#fff;font:20px sans-serif">
<h1 id="h" style="color:#c00;margin:8px">WV2 SPIKE PAGE</h1>
<canvas id="cv" width="300" height="80" style="border:1px solid #000"></canvas>
<div><input id="inp" style="width:400px;font-size:20px"></div>
<div><textarea id="ta" rows="3" cols="40"></textarea></div>
<button id="btn" style="font-size:24px">Click me</button>
<a id="lnk" href="#x" style="font-size:30px;margin-left:30px">A link to hover</a>
<div style="height:3000px;background:linear-gradient(#fff,#00f)">tall content</div>
<script>
var clicks=0, raf=0, animate=true, events=[], wheel=0, wheelTrusted=null, clickTrusted=null, focusEvents=[];
var cv=document.getElementById('cv');
function tick(t){raf++; if(animate){var g=cv.getContext('2d');g.fillStyle='#eee';g.fillRect(0,0,300,80);g.fillStyle='#0a0';g.fillRect((t/5)%260,10,40,60);} requestAnimationFrame(tick);}
requestAnimationFrame(tick);
['keydown','keyup','keypress','beforeinput','input'].forEach(function(ev){document.addEventListener(ev,function(e){events.push([e.type,e.key,e.code,e.isTrusted,e.target.id,e.data,e.ctrlKey])},true)});
window.addEventListener('wheel',function(e){wheel+=e.deltaY;wheelTrusted=e.isTrusted},{passive:true});
document.getElementById('btn').addEventListener('click',function(e){clicks++;clickTrusted=e.isTrusted;this.textContent='clicked '+clicks});
window.addEventListener('focus',function(){focusEvents.push('wfocus')});
window.addEventListener('blur',function(){focusEvents.push('wblur')});
</script></body></html>"##;

fn key(wv: &ICoreWebView2, vk: u32, k: &str, code: &str, text: Option<&str>, mods: u32) -> Vec<String> {
    let mut r = vec![];
    let base = format!(
        r#""windowsVirtualKeyCode":{vk},"nativeVirtualKeyCode":{vk},"key":{},"code":{},"modifiers":{mods}"#,
        serde_q(k),
        serde_q(code)
    );
    r.push(cdp(wv, "Input.dispatchKeyEvent", &format!(r#"{{"type":"rawKeyDown",{base}}}"#)));
    if let Some(t) = text {
        r.push(cdp(
            wv,
            "Input.dispatchKeyEvent",
            &format!(r#"{{"type":"char",{base},"text":{},"unmodifiedText":{}}}"#, serde_q(t), serde_q(t)),
        ));
    }
    r.push(cdp(wv, "Input.dispatchKeyEvent", &format!(r#"{{"type":"keyUp",{base}}}"#)));
    r
}

fn serde_q(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn type_char(wv: &ICoreWebView2, c: char) -> Vec<String> {
    let (vk, code) = match c {
        'a'..='z' => (c.to_ascii_uppercase() as u32, format!("Key{}", c.to_ascii_uppercase())),
        'A'..='Z' => (c as u32, format!("Key{}", c)),
        '0'..='9' => (c as u32, format!("Digit{}", c)),
        ' ' => (0x20, "Space".into()),
        ',' => (0xBC, "Comma".into()),
        '!' => (0x31, "Digit1".into()),
        _ => (0, "".into()),
    };
    let mods = if c.is_ascii_uppercase() || c == '!' { 8 } else { 0 };
    let s = c.to_string();
    key(wv, vk, &s, &code, Some(&s), mods)
}

fn main() -> windows::core::Result<()> {
    let spike = std::env::current_dir().unwrap();
    let udf = spike.join("udf");
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
    }
    log!("Windows: {:?}", os_build());
    unsafe {
        let mut v = PWSTR::null();
        let r = GetAvailableCoreWebView2BrowserVersionString(PCWSTR::null(), &mut v);
        log!("GetAvailableCoreWebView2BrowserVersionString -> {:?} {}", r, v.to_string().unwrap_or_default());
        CoTaskMemFree(Some(v.0 as _));
    }
    log!("foreground at start: {}", fg());

    // Dispatcher queue + compositor
    let _dqc = unsafe {
        CreateDispatcherQueueController(DispatcherQueueOptions {
            dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_NONE,
        })?
    };
    let compositor = Compositor::new()?;
    let root: ContainerVisual = compositor.CreateContainerVisual()?;
    let child: ContainerVisual = compositor.CreateContainerVisual()?;
    root.Children()?.InsertAtTop(&child)?;
    let (mut w, mut h) = (800i32, 600i32);
    root.SetSize(Vector2 { X: w as f32, Y: h as f32 })?;
    child.SetSize(Vector2 { X: w as f32, Y: h as f32 })?;

    // message-only window
    let hwnd = unsafe {
        let inst = GetModuleHandleW(None)?;
        let cls = windows::core::w!("wv2spike_msg");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: cls, ..Default::default() };
        RegisterClassW(&wc);
        CreateWindowExW(WINDOW_EX_STYLE(0), cls, windows::core::w!("wv2spike"), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(inst.into()), None)?
    };
    log!("message-only hwnd = {:?}", hwnd.0);

    // Environment
    let env: ICoreWebView2Environment = {
        let out = Rc::new(RefCell::new(None));
        let o2 = out.clone();
        let udf_h = HSTRING::from(udf.as_os_str());
        CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
            Box::new(move |h| unsafe {
                CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), &udf_h, None, &h)
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |hr, e| {
                hr?;
                *o2.borrow_mut() = e;
                Ok(())
            }),
        )
        .expect("env");
        let e = out.borrow_mut().take();
        e.expect("env none")
    };
    unsafe {
        let mut v = PWSTR::null();
        env.BrowserVersionString(&mut v)?;
        log!("env.BrowserVersionString = {}", v.to_string().unwrap_or_default());
    }
    let env3: ICoreWebView2Environment3 = env.cast()?;
    let comp: ICoreWebView2CompositionController = {
        let out = Rc::new(RefCell::new(None));
        let o2 = out.clone();
        let e3 = env3.clone();
        CreateCoreWebView2CompositionControllerCompletedHandler::wait_for_async_operation(
            Box::new(move |h| unsafe {
                e3.CreateCoreWebView2CompositionController(hwnd, &h)
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |hr, c| {
                hr?;
                *o2.borrow_mut() = c;
                Ok(())
            }),
        )
        .expect("compctl");
        let c = out.borrow_mut().take();
        c.expect("comp none")
    };
    let ctl: ICoreWebView2Controller = comp.cast()?;
    let wv: ICoreWebView2 = unsafe { ctl.CoreWebView2()? };
    unsafe {
        comp.SetRootVisualTarget(&child.cast::<windows::core::IUnknown>()?)?;
        if let Ok(c3) = ctl.cast::<ICoreWebView2Controller3>() {
            let mut s = 0f64;
            let _ = c3.RasterizationScale(&mut s);
            log!("default RasterizationScale={s}");
            c3.SetShouldDetectMonitorScaleChanges(false)?;
            c3.SetRasterizationScale(1.0)?;
        }
        ctl.SetBounds(RECT { left: 0, top: 0, right: w, bottom: h })?;
        ctl.SetIsVisible(true)?;
    }

    // cursor changed handler
    let cursor_log: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(vec![]));
    {
        let cl = cursor_log.clone();
        let mut tok = 0i64;
        unsafe {
            comp.add_CursorChanged(
                &CursorChangedEventHandler::create(Box::new(move |s, _| {
                    if let Some(s) = s {
                        let mut id = 0u32;
                        let _ = s.SystemCursorId(&mut id);
                        cl.borrow_mut().push(id);
                    }
                    Ok(())
                })),
                &mut tok,
            )?;
        }
    }
    // focus changed events (controller)
    let gotfocus = Rc::new(RefCell::new(0u32));
    {
        let g = gotfocus.clone();
        let mut tok = 0i64;
        unsafe {
            ctl.add_GotFocus(
                &FocusChangedEventHandler::create(Box::new(move |_, _| {
                    *g.borrow_mut() += 1;
                    Ok(())
                })),
                &mut tok,
            )?;
        }
    }

    unsafe {
        let p = CoTaskMemPWSTR::from(PAGE);
        wv.NavigateToString(*p.as_ref().as_pcwstr())?;
    }
    for _ in 0..100 {
        pump(100);
        if exec(&wv, "document.readyState") == "\"complete\"" {
            break;
        }
    }
    log!("readyState={} title={}", exec(&wv, "document.readyState"), exec(&wv, "document.title"));

    // D3D + capture
    let mut dev: Option<ID3D11Device> = None;
    let mut ctx: Option<ID3D11DeviceContext> = None;
    unsafe {
        D3D11CreateDevice(None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT, None, D3D11_SDK_VERSION, Some(&mut dev), None, Some(&mut ctx))?;
    }
    let dev = dev.unwrap();
    let ctx = ctx.unwrap();
    unsafe {
        if let Ok(mt) = dev.cast::<ID3D11Multithread>() {
            let _ = mt.SetMultithreadProtected(true);
        }
    }
    let wdev: IDirect3DDevice = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dev.cast::<IDXGIDevice>()?)?.cast()? };
    let shared = Arc::new(Shared {
        count: AtomicU64::new(0),
        times: Mutex::new(vec![]),
        last: Mutex::new((0, 0, 0, 0)),
        save: Mutex::new(None),
        saved: Mutex::new(None),
        d3d: D3D { dev: dev.clone(), ctx: ctx.clone() },
    });
    log!("GraphicsCaptureSession::IsSupported = {:?}", GraphicsCaptureSession::IsSupported());
    let item = GraphicsCaptureItem::CreateFromVisual(&root)?;
    log!("item.Size = {:?} DisplayName={:?}", item.Size(), item.DisplayName());

    if std::env::args().nth(1).as_deref() == Some("followup") {
        let cap = start_capture(&item, &wdev, &shared, w, h, Some(false), Some(false), None)?;
        followup(&wv, &ctl, &comp, &shared, &cursor_log, &gotfocus);
        stop_capture(cap);
        unsafe { let _ = ctl.Close(); }
        pump(300);
        log!("DONE");
        return Ok(());
    }
    // ---- Q1: default session props (border/cursor not touched)
    log!("== Q1 offscreen frames, default session properties");
    let cap = start_capture(&item, &wdev, &shared, w, h, None, None, None)?;
    pump(1500);
    log!("frames after 1.5s: {}", shared.count.load(Ordering::SeqCst));
    log!("{}", request_save(&shared, &spike.join("frame_default_800x600.png").to_string_lossy()));
    stop_capture(cap);

    log!("== Q1b border=false cursor=false");
    let cap = start_capture(&item, &wdev, &shared, w, h, Some(false), Some(false), None)?;
    pump(1000);
    log!("{}", request_save(&shared, &spike.join("frame_noborder_800x600.png").to_string_lossy()));

    // ---- Q2 cadence
    log!("== Q2 cadence");
    measure(&shared, &wv, 10_000, "animating 800x600 10s");
    exec(&wv, "animate=false");
    measure(&shared, &wv, 5_000, "static (rAF loop running, no paint) 5s");
    exec(&wv, "window.__stopRaf=true; tick=function(){}; 0");
    exec(&wv, "animate=true");
    stop_capture(cap);
    // restore a fresh animation
    exec(&wv, "raf=0; animate=true; (function t2(ts){raf++; var g=cv.getContext('2d');g.fillStyle='#eee';g.fillRect(0,0,300,80);g.fillStyle='#00a';g.fillRect((ts/5)%260,10,40,60); if(animate) requestAnimationFrame(t2); else requestAnimationFrame(t2);})(0); 0");
    for ms in [33i64, 100] {
        let cap = start_capture(&item, &wdev, &shared, w, h, Some(false), Some(false), Some(ms))?;
        measure(&shared, &wv, 4_000, &format!("animating MinUpdateInterval={ms}ms"));
        stop_capture(cap);
    }
    let cap = start_capture(&item, &wdev, &shared, w, h, Some(false), Some(false), Some(1))?;
    measure(&shared, &wv, 4_000, "animating MinUpdateInterval=1ms");

    // ---- Q3 keyboard
    log!("== Q3 keyboard");
    log!("fg before input: {}", fg());
    log!("initial: hasFocus={} active={} focusEvents={}", exec(&wv, "document.hasFocus()"), exec(&wv, "document.activeElement.id||document.activeElement.tagName"), exec(&wv, "JSON.stringify(focusEvents)"));
    exec(&wv, "document.getElementById('inp').focus()");
    log!("after inp.focus(): hasFocus={} active={} :focus={} gotfocus_evts={}", exec(&wv, "document.hasFocus()"), exec(&wv, "document.activeElement.id"), exec(&wv, "document.getElementById('inp').matches(':focus')"), gotfocus.borrow());
    let mut errs = vec![];
    for c in "Hello, World!".chars() {
        errs.extend(type_char(&wv, c).into_iter().filter(|s| s.starts_with("ERR")));
    }
    log!("after typing: value={} errs={:?} fg={}", exec(&wv, "document.getElementById('inp').value"), errs, fg());
    key(&wv, 8, "Backspace", "Backspace", None, 0);
    log!("after Backspace: {}", exec(&wv, "document.getElementById('inp').value"));
    key(&wv, 37, "ArrowLeft", "ArrowLeft", None, 0);
    key(&wv, 37, "ArrowLeft", "ArrowLeft", None, 0);
    type_char(&wv, 'X');
    log!("after 2xArrowLeft + X: {} sel={}", exec(&wv, "document.getElementById('inp').value"), exec(&wv, "[inp.selectionStart,inp.selectionEnd].join()"));
    // Ctrl+A
    cdp(&wv, "Input.dispatchKeyEvent", r#"{"type":"rawKeyDown","windowsVirtualKeyCode":17,"key":"Control","code":"ControlLeft","modifiers":2}"#);
    key(&wv, 65, "a", "KeyA", None, 2);
    cdp(&wv, "Input.dispatchKeyEvent", r#"{"type":"keyUp","windowsVirtualKeyCode":17,"key":"Control","code":"ControlLeft","modifiers":0}"#);
    log!("after Ctrl+A: sel={} len={}", exec(&wv, "[inp.selectionStart,inp.selectionEnd].join()"), exec(&wv, "inp.value.length"));
    key(&wv, 9, "Tab", "Tab", None, 0);
    log!("after Tab: active={}", exec(&wv, "document.activeElement.id"));
    type_char(&wv, 'a');
    type_char(&wv, 'b');
    key(&wv, 13, "Enter", "Enter", Some("\r"), 0);
    type_char(&wv, 'c');
    log!("insertText -> {}", cdp(&wv, "Input.insertText", r#"{"text":"é😀"}"#));
    log!("textarea value={}", exec(&wv, "JSON.stringify(document.getElementById('ta').value)"));
    log!("events count={} untrusted={} sample={}", exec(&wv, "events.length"), exec(&wv, "events.filter(e=>!e[3]).length"),
        exec(&wv, "JSON.stringify(events.filter(e=>e[0]=='keydown').slice(0,4).concat(events.filter(e=>e[0]=='keydown'&&(e[1]=='Backspace'||e[1]=='Tab'||e[1]=='Enter'||e[1]=='ArrowLeft'||e[1]=='a'&&e[6]))).concat(events.filter(e=>e[0]=='input').slice(-3)))"));
    log!("hasFocus before emulation={} :focus(ta)={}", exec(&wv, "document.hasFocus()"), exec(&wv, "document.getElementById('ta').matches(':focus')"));
    log!("Emulation.setFocusEmulationEnabled -> {}", cdp(&wv, "Emulation.setFocusEmulationEnabled", r#"{"enabled":true}"#));
    pump(200);
    log!("hasFocus after emulation={} :focus(ta)={} :focus-visible={} focusEvents={}", exec(&wv, "document.hasFocus()"), exec(&wv, "document.getElementById('ta').matches(':focus')"), exec(&wv, "document.getElementById('ta').matches(':focus-visible')"), exec(&wv, "JSON.stringify(focusEvents)"));
    log!("fg after keyboard: {}", fg());

    // ---- Q4 mouse
    log!("== Q4 mouse");
    let rect = exec(&wv, "(function(){var r=document.getElementById('btn').getBoundingClientRect();return [Math.round(r.x+r.width/2),Math.round(r.y+r.height/2)].join()})()");
    let rect = rect.trim_matches('"').to_string();
    let v: Vec<i32> = rect.split(',').filter_map(|s| s.parse().ok()).collect();
    let pt = POINT { x: v[0], y: v[1] };
    log!("button center {:?}; fg before: {}", (pt.x, pt.y), fg());
    unsafe {
        log!("move {:?}", comp.SendMouseInput(COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE, 0, pt));
        log!("down {:?}", comp.SendMouseInput(COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_LEFT_BUTTON, 0, pt));
        log!("up {:?}", comp.SendMouseInput(COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE, 0, pt));
    }
    pump(300);
    log!("clicks={} clickTrusted={} btnText={} active={} gotfocus_evts={} fg after: {}", exec(&wv, "clicks"), exec(&wv, "clickTrusted"), exec(&wv, "document.getElementById('btn').textContent"), exec(&wv, "document.activeElement.id"), gotfocus.borrow(), fg());
    unsafe {
        for _ in 0..3 {
            log!("wheel {:?}", comp.SendMouseInput(COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE, (-120i32) as u32, POINT { x: 400, y: 400 }));
            pump(100);
        }
    }
    pump(500);
    log!("after 3 wheel notches: scrollY={} wheelSum={} wheelTrusted={} fg={}", exec(&wv, "window.scrollY"), exec(&wv, "wheel"), exec(&wv, "wheelTrusted"), fg());
    log!("{}", request_save(&shared, &spike.join("frame_scrolled.png").to_string_lossy()));
    exec(&wv, "window.scrollTo(0,0)");
    pump(300);

    // ---- Q5 cursor
    log!("== Q5 cursor");
    let lr = exec(&wv, "(function(){var r=document.getElementById('lnk').getBoundingClientRect();return [Math.round(r.x+r.width/2),Math.round(r.y+r.height/2)].join()})()");
    let v: Vec<i32> = lr.trim_matches('"').split(',').filter_map(|s| s.parse().ok()).collect();
    let ir = exec(&wv, "(function(){var r=document.getElementById('inp').getBoundingClientRect();return [Math.round(r.x+r.width/2),Math.round(r.y+r.height/2)].join()})()");
    let vi: Vec<i32> = ir.trim_matches('"').split(',').filter_map(|s| s.parse().ok()).collect();
    for (name, p) in [("blank", POINT { x: 700, y: 20 }), ("link", POINT { x: v[0], y: v[1] }), ("input", POINT { x: vi[0], y: vi[1] }), ("blank", POINT { x: 700, y: 20 })] {
        unsafe {
            let _ = comp.SendMouseInput(COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE, 0, p);
        }
        pump(400);
        let mut id = 0u32;
        let mut hc = HCURSOR::default();
        unsafe {
            let _ = comp.SystemCursorId(&mut id);
            let _ = comp.Cursor(&mut hc);
        }
        log!("hover {name} {:?}: SystemCursorId={id} HCURSOR={:?} events so far={:?}", (p.x, p.y), hc.0, cursor_log.borrow());
    }

    // ---- Q6 resize
    log!("== Q6 resize to 1024x768");
    w = 1024;
    h = 768;
    unsafe {
        ctl.SetBounds(RECT { left: 0, top: 0, right: w, bottom: h })?;
    }
    root.SetSize(Vector2 { X: w as f32, Y: h as f32 })?;
    child.SetSize(Vector2 { X: w as f32, Y: h as f32 })?;
    pump(300);
    log!("item.Size after visual resize = {:?}; last frame before Recreate={:?}", item.Size(), *shared.last.lock().unwrap());
    log!("Recreate -> {:?}", cap.pool.Recreate(&wdev, DirectXPixelFormat::B8G8R8A8UIntNormalized, 2, SizeInt32 { Width: w, Height: h }));
    pump(500);
    log!("page innerWidth x innerHeight = {}", exec(&wv, "innerWidth+'x'+innerHeight"));
    log!("{}", request_save(&shared, &spike.join("frame_1024x768.png").to_string_lossy()));
    measure(&shared, &wv, 3_000, "animating 1024x768");

    // ---- Q7 visibility
    log!("== Q7 visibility");
    unsafe {
        ctl.SetIsVisible(false)?;
    }
    pump(300);
    log!("visibilityState={} hidden={}", exec(&wv, "document.visibilityState"), exec(&wv, "document.hidden"));
    measure(&shared, &wv, 4_000, "IsVisible=FALSE");
    unsafe {
        ctl.SetIsVisible(true)?;
    }
    pump(300);
    log!("visibilityState={}", exec(&wv, "document.visibilityState"));
    measure(&shared, &wv, 4_000, "IsVisible=TRUE again");
    log!("{}", request_save(&shared, &spike.join("frame_after_visible.png").to_string_lossy()));

    log!("fg at end: {}", fg());
    stop_capture(cap);
    unsafe {
        let _ = ctl.Close();
    }
    pump(500);
    log!("DONE");
    Ok(())
}

fn os_build() -> String {
    let out = std::process::Command::new("C:\\Windows\\System32\\cmd.exe").args(["/c", "ver"]).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

extern "system" fn wndproc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(h, m, w, l) }
}

fn center(wv: &ICoreWebView2, id: &str) -> POINT {
    let r = exec(wv, &format!("(function(){{var r=document.getElementById('{id}').getBoundingClientRect();return [Math.round(r.x+r.width/2),Math.round(r.y+r.height/2)].join()}})()"));
    let v: Vec<i32> = r.trim_matches('"').split(',').filter_map(|s| s.parse().ok()).collect();
    POINT { x: v[0], y: v[1] }
}

fn followup(
    wv: &ICoreWebView2,
    ctl: &ICoreWebView2Controller,
    comp: &ICoreWebView2CompositionController,
    shared: &Arc<Shared>,
    cursor_log: &Rc<RefCell<Vec<u32>>>,
    gotfocus: &Rc<RefCell<u32>>,
) {
    let st = |l: &str| {
        log!("[{l}] hasFocus={} active={} focusEvents={} ctlGotFocus={} fg={}",
            exec(wv, "document.hasFocus()"), exec(wv, "document.activeElement.id||document.activeElement.tagName"),
            exec(wv, "JSON.stringify(focusEvents)"), gotfocus.borrow(), fg());
    };
    log!("args env WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS={:?}", std::env::var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS").ok());
    measure(shared, wv, 5_000, "followup animating 800x600");
    st("start");
    exec(wv, "document.getElementById('inp').focus()");
    st("after inp.focus()");
    type_char(wv, 'q');
    pump(200);
    st("after one CDP key");
    log!("inp value={}", exec(wv, "inp.value"));
    // CDP mouse
    let b = center(wv, "btn");
    for t in ["mouseMoved", "mousePressed", "mouseReleased"] {
        let r = cdp(wv, "Input.dispatchMouseEvent", &format!(r#"{{"type":"{t}","x":{},"y":{},"button":"left","buttons":{},"clickCount":1}}"#, b.x, b.y, if t == "mousePressed" { 1 } else { 0 }));
        log!("  CDP {t} -> {r}");
    }
    pump(300);
    log!("CDP click: clicks={} clickTrusted={} active={}", exec(wv, "clicks"), exec(wv, "clickTrusted"), exec(wv, "document.activeElement.id"));
    st("after CDP click");
    log!("CDP wheel -> {}", cdp(wv, "Input.dispatchMouseEvent", r#"{"type":"mouseWheel","x":400,"y":400,"deltaX":0,"deltaY":240}"#));
    pump(500);
    log!("after CDP wheel: scrollY={} wheel={} trusted={}", exec(wv, "scrollY"), exec(wv, "wheel"), exec(wv, "wheelTrusted"));
    exec(wv, "scrollTo(0,0)");
    pump(300);
    // cursor lag check (SendMouseInput MOVE only)
    let l = center(wv, "lnk");
    let i = center(wv, "inp");
    for (name, p) in [("blank", POINT { x: 700, y: 20 }), ("link", l), ("link+1", POINT { x: l.x + 1, y: l.y }), ("input", i), ("input+1", POINT { x: i.x + 1, y: i.y }), ("blank", POINT { x: 700, y: 20 }), ("blank+1", POINT { x: 701, y: 20 })] {
        let n0 = cursor_log.borrow().len();
        unsafe { let _ = comp.SendMouseInput(COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE, 0, p); }
        let t0 = Instant::now();
        let mut when = None;
        while t0.elapsed() < Duration::from_millis(1500) {
            pump(10);
            if when.is_none() && cursor_log.borrow().len() > n0 { when = Some(t0.elapsed().as_millis()); }
        }
        let mut id = 0u32;
        unsafe { let _ = comp.SystemCursorId(&mut id); }
        log!("SendMouseInput MOVE {name} {:?}: SystemCursorId={id} newEvents={:?} firstEventAfterMs={:?} fg={}", (p.x, p.y), &cursor_log.borrow()[n0..], when, fg());
    }
    log!("CSS cursor at link: {}", exec(wv, "getComputedStyle(document.getElementById('lnk')).cursor"));
    // CDP-driven hover: does CursorChanged fire?
    let n0 = cursor_log.borrow().len();
    cdp(wv, "Input.dispatchMouseEvent", &format!(r#"{{"type":"mouseMoved","x":{},"y":{}}}"#, l.x, l.y));
    pump(800);
    let mut id = 0u32;
    unsafe { let _ = comp.SystemCursorId(&mut id); }
    log!("CDP mouseMoved over link: SystemCursorId={id} newEvents={:?}", &cursor_log.borrow()[n0..]);
    // visibility
    unsafe { let _ = ctl.SetIsVisible(false); }
    pump(1500);
    log!("hidden 1.5s: visibilityState={} raf={}", exec(wv, "document.visibilityState"), exec(wv, "raf"));
    pump(1000);
    log!("hidden 2.5s: raf={}", exec(wv, "raf"));
    unsafe { let _ = ctl.SetIsVisible(true); }
    pump(500);
    st("visible again");
    // Which input steals foreground?
    unsafe { let _ = ctl.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC); }
    pump(300);
    st("after controller.MoveFocus(PROGRAMMATIC)");
}
