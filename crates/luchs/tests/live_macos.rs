//! Requires scripts/build-helper.sh and a logged-in macOS desktop.
#![cfg(target_os = "macos")]

use jackstay::{
    acquisition::{arena::AcquireOutcome, socket::CpuSetupClient},
    bootstrap::{ChannelRequest, InputRequest, connect_v2},
    local::Stream,
};
use luchs::helper::{CommandOutcome, Helper};
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn page(path: &std::path::Path, color: &str) {
    std::fs::write(
        path,
        format!("<!doctype html><style>html,body{{margin:0;background:{color}}}</style>"),
    )
    .unwrap();
}

// Copying out a completed reservation is test inspection only; the production
// CLI commits the same bytes in place. Drop the helper before its writer export.
struct NativeCapture {
    helper: Helper,
    arena: jackstay::acquisition::arena::ArenaProducer,
    export: Option<jackstay::acquisition::arena::WriterExport>,
    size: (u32, u32),
}
impl std::ops::Deref for NativeCapture {
    type Target = Helper;
    fn deref(&self) -> &Helper {
        &self.helper
    }
}
impl std::ops::DerefMut for NativeCapture {
    fn deref_mut(&mut self) -> &mut Helper {
        &mut self.helper
    }
}
impl NativeCapture {
    fn new(helper: Helper, width: u32, height: u32) -> Self {
        use jackstay::acquisition::arena::{ArenaConfig, ArenaProducer};
        let arena = ArenaProducer::new(ArenaConfig {
            resource_capacity: 8,
            retained_history: 1,
            producer_reserve: 1,
            payload_capacity: width as usize * height as usize * 4,
            memory_budget: 1 << 30,
            max_incarnations: 1,
            drain_timeout: TIMEOUT,
        })
        .unwrap();
        Self {
            helper,
            arena,
            export: None,
            size: (width, height),
        }
    }
    fn capture(&mut self, size: (u32, u32)) -> Option<luchs::protocol::Frame> {
        use luchs::protocol::{AckOutcome, Format, Frame, Header};
        if size != self.size {
            self.arena
                .reconfigure_cpu(size.0 as usize * size.1 as usize * 4)
                .unwrap();
            self.size = size;
            self.install();
        } else if self.export.is_none() {
            self.install();
        }
        let mut reservation = self.arena.reserve().unwrap().unwrap();
        let slot = reservation.slot();
        let pending = self
            .helper
            .send_json_command(
                serde_json::json!({"type":"draw", "arena_scope":slot.arena_scope, "slot":slot.slot,
            "generation":slot.generation, "width":size.0, "height":size.1, "stride":size.0 * 4}),
                TIMEOUT,
            )
            .unwrap();
        let ack = pending.wait_ack().unwrap();
        assert_eq!(ack.outcome, AckOutcome::Executed, "{:?}", ack.detail);
        assert_eq!(ack.generation, Some(slot.generation));
        assert_eq!(ack.slot, Some(slot.slot));
        let result = if ack.capture.is_some_and(|r| r.published) {
            let header = Header {
                format: Format::Bgra8,
                width: size.0,
                height: size.1,
                stride: size.0 * 4,
                len: size.0 as usize * size.1 as usize * 4,
            };
            assert_eq!(ack.frame, Some(header));
            let header = ack.frame.unwrap();
            Some(Frame {
                pixels: reservation.bytes_mut()[..header.len].to_vec(),
                header,
            })
        } else {
            None
        };
        self.arena.abandon(reservation).unwrap();
        result
    }
    fn install(&mut self) {
        use std::os::fd::AsFd;
        let export = self.arena.export_writer().unwrap().unwrap();
        // SAFETY: export retained until replacement ack or helper destruction.
        let fd = unsafe { export.duplicate_object() }.unwrap();
        let ack = self
            .helper
            .command_sender()
            .send_with_fd(
                serde_json::json!({"type":"arena", "layout":export.descriptor()}),
                TIMEOUT,
                Some(fd.as_fd()),
            )
            .unwrap()
            .wait_ack()
            .unwrap();
        assert_eq!(ack.generation, Some(export.descriptor().generation));
        self.export = Some(export);
    }
}
fn request_frame(helper: &mut NativeCapture) -> luchs::protocol::Frame {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(frame) = helper.capture(helper.size) {
            return frame;
        }
        assert!(Instant::now() < deadline, "no native frame");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
#[ignore = "requires built Swift helper and a live macOS desktop"]
fn native_writer_rejects_stale_and_foreign_slots_after_reexport() {
    use luchs::protocol::AckOutcome;
    let dir = tempfile::tempdir().unwrap();
    let html = dir.path().join("page.html");
    page(&html, "red");
    let renderer =
        std::path::Path::new(env!("CARGO_BIN_EXE_luchs")).with_file_name("luchs-webview-capture");
    let helper = Helper::spawn(
        Command::new(renderer)
            .arg(html)
            .args(["32", "32", "0", "15"]),
    )
    .unwrap();
    let mut helper = NativeCapture::new(helper, 32, 32);
    assert_eq!(&request_frame(&mut helper).pixels[..4], &[0, 0, 255, 255]);
    let reservation = helper.arena.reserve().unwrap().unwrap();
    let slot = reservation.slot();
    let mut valid = serde_json::json!({"type":"draw", "arena_scope":slot.arena_scope,
        "generation":slot.generation, "slot":slot.slot, "width":32, "height":32, "stride":128});
    // Exercise validation after a context has been cached for this slot too.
    assert_eq!(
        helper
            .send_json_command(valid.clone(), TIMEOUT)
            .unwrap()
            .wait_ack()
            .unwrap()
            .outcome,
        AckOutcome::Executed
    );
    for field in ["generation", "arena_scope", "slot"] {
        let mut invalid = valid.clone();
        match field {
            "generation" => invalid[field] = (slot.generation + 1).into(),
            "arena_scope" => {
                let mut foreign = slot.arena_scope;
                foreign[0] ^= 1;
                invalid[field] = serde_json::json!(foreign);
            }
            _ => invalid[field] = u32::MAX.into(),
        }
        assert_eq!(
            helper
                .send_json_command(invalid, TIMEOUT)
                .unwrap()
                .wait_ack()
                .unwrap()
                .outcome,
            AckOutcome::Failed,
            "{field}"
        );
    }
    helper.arena.abandon(reservation).unwrap();
    assert_eq!(
        helper
            .send_json_command(
                serde_json::json!({"type":"resize",
        "width":48,"height":48,"scale":1}),
                TIMEOUT
            )
            .unwrap()
            .wait(),
        CommandOutcome::Executed
    );
    // NativeCapture installs the replacement while the old export is retained.
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(frame) = helper.capture((48, 48)) {
            assert_eq!(&frame.pixels[..4], &[0, 0, 255, 255]);
            break;
        }
        assert!(Instant::now() < deadline, "no resized native frame");
        std::thread::sleep(Duration::from_millis(20));
    }
    valid["width"] = 48.into();
    valid["height"] = 48.into();
    valid["stride"] = 192.into();
    assert_eq!(
        helper
            .send_json_command(valid, TIMEOUT)
            .unwrap()
            .wait_ack()
            .unwrap()
            .outcome,
        AckOutcome::Failed
    );
    assert_eq!(helper.command("ping", TIMEOUT), CommandOutcome::Executed);
    let generation = helper.export.as_ref().unwrap().descriptor().generation;
    assert_ne!(generation, slot.generation);
    assert_eq!(
        helper
            .send_json_command(
                serde_json::json!({"type":"arena_release",
        "generation":slot.generation}),
                TIMEOUT
            )
            .unwrap()
            .wait(),
        CommandOutcome::Failed(Some("stale arena release".into()))
    );
    assert_eq!(
        helper
            .send_json_command(
                serde_json::json!({"type":"arena_release",
        "generation":generation}),
                TIMEOUT
            )
            .unwrap()
            .wait(),
        CommandOutcome::Executed
    );
    helper.export = None;
    assert_eq!(helper.command("ping", TIMEOUT), CommandOutcome::Executed);
    assert_eq!(&request_frame(&mut helper).pixels[..4], &[0, 0, 255, 255]);
}

#[test]
#[ignore = "requires built Swift helper and a live macOS desktop"]
fn native_ping_reload_and_watch() {
    let dir = tempfile::tempdir().unwrap();
    let html = dir.path().join("page.html");
    page(&html, "red");
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_luchs"));
    let renderer = binary.with_file_name("luchs-webview-capture");
    assert!(renderer.exists(), "run scripts/build-helper.sh first");
    let helper = Helper::spawn(
        Command::new(&renderer)
            .arg(&html)
            .args(["32", "32", "0", "15"]),
    )
    .unwrap();
    let mut helper = NativeCapture::new(helper, 32, 32);
    let initial = request_frame(&mut helper);
    assert_eq!(initial.header.width, 32);
    assert_eq!(&initial.pixels[..4], &[0, 0, 255, 255]);
    assert_eq!(helper.command("ping", TIMEOUT), CommandOutcome::Executed);
    assert_eq!(
        helper.command("old_sdl_input", TIMEOUT),
        CommandOutcome::Unsupported
    );
    page(&html, "blue");
    assert_eq!(helper.reload().unwrap(), CommandOutcome::Executed);
    let deadline = Instant::now() + TIMEOUT;
    std::thread::sleep(Duration::from_millis(200));
    loop {
        let frame = request_frame(&mut helper);
        if frame.pixels[..4] == [255, 0, 0, 255] {
            break;
        }
        assert!(Instant::now() < deadline, "reload did not change pixels");
    }
    drop(helper);

    // Exercise --watch through the real CLI, helper and Jackstay producer.
    page(&html, "red");
    let mut process = Process(
        Command::new(binary)
            .arg("--helper")
            .arg(&renderer)
            .args(["--size=32x32", "--watch", "--fps=15"])
            .arg(&html)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut endpoint = String::new();
    BufReader::new(process.0.stdout.take().unwrap())
        .read_line(&mut endpoint)
        .unwrap();
    let connected = connect_v2(
        Stream::connect(endpoint.trim()).unwrap(),
        InputRequest::None,
        ChannelRequest::None,
    )
    .unwrap();
    // SAFETY: the child CLI is the conforming sole producer of this grant.
    let mut setup = unsafe { CpuSetupClient::from_stream(connected.media) };
    let consumer = setup.attach(1).unwrap();
    for color in [[0, 0, 255, 255], [255, 0, 0, 255]] {
        if color[0] == 255 {
            page(&html, "blue");
            std::fs::File::options()
                .write(true)
                .open(&html)
                .unwrap()
                .set_modified(std::time::SystemTime::now() + Duration::from_secs(2))
                .unwrap();
        }
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let AcquireOutcome::Frame(frame) = consumer.acquire_latest(0).unwrap() {
                if frame.bytes()[..4] == color {
                    break;
                }
            }
            assert!(Instant::now() < deadline, "watch did not publish {color:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    drop((consumer, setup));
    // SAFETY: this test owns the unreaped child process.
    unsafe {
        libc::kill(process.0.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = process.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "CLI did not stop");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!std::path::Path::new(endpoint.trim()).exists());
}

#[test]
#[ignore = "requires built Swift helper and a live macOS desktop"]
fn native_socket_eof_exits_successfully() {
    use luchs::protocol::{Record, encode_command, read_record};
    use std::{
        io::Write,
        os::{
            fd::AsRawFd,
            unix::{net::UnixStream, process::CommandExt},
        },
        sync::mpsc,
    };
    let (mut socket, inherited) = UnixStream::pair().unwrap();
    let fd = inherited.as_raw_fd();
    let dir = tempfile::tempdir().unwrap();
    let html = dir.path().join("page.html");
    page(&html, "red");
    let renderer =
        std::path::Path::new(env!("CARGO_BIN_EXE_luchs")).with_file_name("luchs-webview-capture");
    let mut command = Command::new(renderer);
    command.env("LUCHS_HELPER_FD", fd.to_string());
    // SAFETY: the child changes only descriptor flags between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut process = Process(
        command
            .arg(html)
            .args(["32", "32", "0", "15"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    drop(inherited);
    let output = socket.try_clone().unwrap();
    let (send, receive) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut stdout = BufReader::new(output);
        while let Some(record) = read_record(&mut stdout).unwrap() {
            if let Record::Ack(ack) = record {
                send.send(ack).unwrap();
            }
        }
    });
    socket
        .write_all(&encode_command(u64::MAX, "ping").unwrap())
        .unwrap();
    let ack = receive.recv_timeout(TIMEOUT).unwrap();
    assert_eq!(ack.id, u64::MAX);
    assert_eq!(ack.outcome, luchs::protocol::AckOutcome::Executed);
    socket.shutdown(std::net::Shutdown::Write).unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = process.0.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Swift helper did not exit on socket EOF"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    reader.join().unwrap();
}

#[test]
#[ignore = "requires built Swift helper and a live macOS desktop"]
fn native_skip_scale_and_hidden_capture() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let html = dir.path().join("page.html");
    page(&html, "red");
    let renderer =
        std::path::Path::new(env!("CARGO_BIN_EXE_luchs")).with_file_name("luchs-webview-capture");
    let helper = Helper::spawn(
        Command::new(renderer)
            .arg(html)
            .args(["32", "32", "0", "30"]),
    )
    .unwrap();
    let mut helper = NativeCapture::new(helper, 32, 32);
    assert_eq!(request_frame(&mut helper).header.width, 32);
    assert!(helper.capture(helper.size).is_none());
    assert_eq!(
        helper
            .send_json_command(
                json!({"type":"presentation", "scale":1., "visible":false}),
                TIMEOUT
            )
            .unwrap()
            .wait(),
        CommandOutcome::Executed
    );
    assert!(helper.capture((32, 32)).is_none());
    assert_eq!(
        helper
            .send_json_command(
                json!({"type":"presentation", "scale":1., "visible":true}),
                TIMEOUT
            )
            .unwrap()
            .wait(),
        CommandOutcome::Executed
    );
    // NativeCapture abandoned the first changed draw. Re-show must reset the
    // helper fingerprint even though this mapping, scale and pixels are identical.
    let frame = helper.capture((32, 32)).unwrap();
    assert_eq!(&frame.pixels[..4], &[0, 0, 255, 255]);
    assert_eq!(
        helper
            .send_json_command(
                json!({"type":"presentation", "scale":2., "visible":true}),
                TIMEOUT
            )
            .unwrap()
            .wait(),
        CommandOutcome::Executed
    );
    let frame = helper.capture((64, 64)).unwrap();
    assert_eq!((frame.header.width, frame.header.height), (64, 64));
    assert_eq!(&frame.pixels[..4], &[0, 0, 255, 255]);
}

#[test]
#[ignore = "requires built Swift helper and a live macOS desktop"]
fn native_fractional_scale_preserves_odd_pixel_sizes() {
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    let html = dir.path().join("page.html");
    page(&html, "red");
    let renderer =
        std::path::Path::new(env!("CARGO_BIN_EXE_luchs")).with_file_name("luchs-webview-capture");
    let helper = Helper::spawn(
        Command::new(renderer)
            .arg(html)
            .args(["17", "11", "0", "30"]),
    )
    .unwrap();
    let mut helper = NativeCapture::new(helper, 17, 11);
    for (scale, expected) in [(1., (17, 11)), (1.5, (26, 17)), (2., (34, 22))] {
        assert_eq!(
            helper
                .send_json_command(
                    json!({"type":"presentation", "scale":scale, "visible":true}),
                    TIMEOUT
                )
                .unwrap()
                .wait(),
            CommandOutcome::Executed
        );
        let deadline = Instant::now() + TIMEOUT;
        let frame = loop {
            if let Some(frame) = helper.capture(expected) {
                break frame;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!((frame.header.width, frame.header.height), expected);
        assert_eq!(&frame.pixels[..4], &[0, 0, 255, 255]);
    }
}

#[test]
#[ignore = "requires built Swift helper and a live macOS desktop"]
fn native_capture_during_slow_load_does_not_block_commands() {
    use std::{io::Read, net::TcpListener, sync::mpsc};
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", server.local_addr().unwrap());
    let (ready, waiting) = mpsc::channel();
    let serve = std::thread::spawn(move || {
        // WebKit can open and abandon a speculative connection before GET.
        let _socket = loop {
            let (mut socket, _) = server.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut request = [0; 4096];
            if matches!(socket.read(&mut request), Ok(n) if n > 0) {
                break socket;
            }
        };
        ready.send(()).unwrap();
        // Leave navigation outstanding, then close without an HTTP response.
        std::thread::sleep(Duration::from_secs(2));
    });
    let renderer =
        std::path::Path::new(env!("CARGO_BIN_EXE_luchs")).with_file_name("luchs-webview-capture");
    let helper = Helper::spawn(
        Command::new(renderer)
            .arg(url)
            .args(["32", "32", "0", "30"]),
    )
    .unwrap();
    let mut helper = NativeCapture::new(helper, 32, 32);
    waiting.recv_timeout(TIMEOUT).unwrap();
    let start = Instant::now();
    assert!(helper.capture((32, 32)).is_none());
    assert_eq!(
        helper.command("ping", Duration::from_secs(1)),
        CommandOutcome::Executed
    );
    assert_eq!(helper.reload().unwrap(), CommandOutcome::Executed);
    assert_eq!(
        helper
            .send_json_command(
                serde_json::json!({"type":"presentation", "scale":1., "visible":false}),
                Duration::from_secs(1)
            )
            .unwrap()
            .wait(),
        CommandOutcome::Executed
    );
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "loading stalled later commands"
    );
    serve.join().unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while !helper.ended() {
        assert!(
            Instant::now() < deadline,
            "navigation failure left helper stuck"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        helper.finish().is_err(),
        "first navigation failure must report an error"
    );
}

#[test]
#[ignore = "requires built Swift helper and a live macOS desktop"]
fn native_input_modes_cleanup_and_precise_scroll() {
    use jackstay::input::*;
    let dir = tempfile::tempdir().unwrap();
    let html = dir.path().join("input.html");
    let log = dir.path().join("console.log");
    std::fs::write(&html, r#"<!doctype html><meta charset="utf-8">
<style>body{margin:0}input{position:absolute;left:0;top:0;width:300px;height:40px}#scroll{position:absolute;top:100px;left:0;width:300px;height:150px;overflow:auto}</style>
<input id="input"><div id="scroll"><div style="height:2000px">long content</div></div>
<script>
input.addEventListener('input',e=>console.log('VALUE '+input.value+' trusted='+e.isTrusted));
for(const kind of ['keydown','keyup','mousedown','mouseup']) document.addEventListener(kind,e=>console.log('EVENT '+kind+' '+(e.key||e.button)+' trusted='+e.isTrusted+' meta='+e.metaKey));
const scroller=document.querySelector('#scroll'); scroller.addEventListener('scroll',()=>console.log('SCROLL '+scroller.scrollTop));
</script>"#).unwrap();
    let renderer =
        std::path::Path::new(env!("CARGO_BIN_EXE_luchs")).with_file_name("luchs-webview-capture");
    let helper = Helper::spawn(
        Command::new(renderer)
            .arg(&html)
            .args(["800", "600", "0", "15"])
            .env("LUCHS_CONSOLE_LOG", &log)
            .env("LUCHS_INPUT_TRACE", "1"),
    )
    .unwrap();
    let mut helper = NativeCapture::new(helper, 800, 600);
    request_frame(&mut helper);
    let mut executor = luchs::input::Executor::new(600.0);
    executor.attach(helper.command_sender());
    let mut id = 0;
    let mut execute = |mode, operation| {
        id += 1;
        assert_eq!(
            executor.execute(Work {
                mode,
                id,
                controller: 1,
                epoch: 1,
                sequence: id,
                operation
            }),
            Outcome::Executed
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    let p = Position {
        revision: 1,
        x: 60.125,
        y: 20.875,
    };
    for action in [Action::Down, Action::Up] {
        execute(
            Mode::SourceText,
            Operation::Event(Event::Button {
                button: 1,
                action,
                position: p,
            }),
        );
    }
    execute(
        Mode::SourceText,
        Operation::Event(Event::Text("é🙂".into())),
    );
    execute(
        Mode::Cooperative,
        Operation::Event(Event::Key {
            press: 1,
            action: Action::Down,
            key: Key::Logical("z".into()),
            modifiers: 0,
        }),
    );
    execute(Mode::Cooperative, Operation::Event(Event::Text("z".into())));
    execute(
        Mode::Cooperative,
        Operation::Event(Event::Key {
            press: 1,
            action: Action::Up,
            key: Key::Logical("z".into()),
            modifiers: 0,
        }),
    );
    execute(
        Mode::Physical,
        Operation::Event(Event::Key {
            press: 2,
            action: Action::Down,
            key: Key::Physical("KeyX".into()),
            modifiers: 0,
        }),
    );
    execute(
        Mode::Physical,
        Operation::Event(Event::Key {
            press: 2,
            action: Action::Up,
            key: Key::Physical("KeyX".into()),
            modifiers: 0,
        }),
    );
    // Shortcut native key-equivalent path: select, copy, then paste once.
    for (press, key) in [(3, "a"), (4, "c"), (5, "v")] {
        for action in [Action::Down, Action::Up] {
            execute(
                Mode::Cooperative,
                Operation::Event(Event::Key {
                    press,
                    action,
                    key: Key::Logical(key.into()),
                    modifiers: 8,
                }),
            );
        }
    }
    execute(
        Mode::Cooperative,
        Operation::Event(Event::Key {
            press: 8,
            action: Action::Down,
            key: Key::Logical("ArrowRight".into()),
            modifiers: 0,
        }),
    );
    execute(
        Mode::Cooperative,
        Operation::Event(Event::Key {
            press: 8,
            action: Action::Up,
            key: Key::Logical("ArrowRight".into()),
            modifiers: 0,
        }),
    );
    for action in [Action::Down, Action::Up] {
        execute(
            Mode::Cooperative,
            Operation::Event(Event::Key {
                press: 9,
                action,
                key: Key::Logical("v".into()),
                modifiers: 8,
            }),
        );
    }
    // Native cleanup releases a delivered key and secondary button, but drops
    // a deferred printable key without typing it during focus loss.
    execute(
        Mode::Physical,
        Operation::Event(Event::Key {
            press: 6,
            action: Action::Down,
            key: Key::Physical("KeyQ".into()),
            modifiers: 0,
        }),
    );
    execute(
        Mode::Cooperative,
        Operation::Event(Event::Key {
            press: 7,
            action: Action::Down,
            key: Key::Logical("w".into()),
            modifiers: 0,
        }),
    );
    execute(
        Mode::Cooperative,
        Operation::Event(Event::Button {
            button: 2,
            action: Action::Down,
            position: p,
        }),
    );
    execute(
        Mode::Cooperative,
        Operation::Cleanup {
            scope: Scope::All,
            reason: Reason::Focus,
        },
    );
    for (unit, y) in [
        (ScrollUnit::Pixel, 0.25),
        (ScrollUnit::Pixel, 0.25),
        (ScrollUnit::Pixel, 0.25),
        (ScrollUnit::Pixel, 0.25),
        (ScrollUnit::Pixel, 80.5),
        (ScrollUnit::Line, 0.5),
        (ScrollUnit::Page, 0.25),
    ] {
        execute(
            Mode::SourceText,
            Operation::Event(Event::Scroll {
                x: 0.0,
                y,
                unit,
                position: Position { y: 150.25, ..p },
            }),
        );
    }
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let text = std::fs::read_to_string(&log).unwrap();
        if text.contains("VALUE é🙂zxé🙂zx")
            && text.contains("EVENT keyup q trusted=true")
            && text.contains("SCROLL ")
        {
            assert!(text.contains("EVENT keydown z trusted=true"));
            assert!(text.contains("button=2 down=false"));
            assert!(text.contains("fixed=-0.25"));
            assert!(
                text.contains("dy=20.0 fixed=-20.0 native=0.0,-20.0 precise=true"),
                "{text}"
            );
            assert!(
                text.contains("dy=150.0 fixed=-150.0 native=0.0,-150.0 precise=true"),
                "{text}"
            );
            assert!(
                text.contains("dy=0.25") && text.contains("native=0.0,-1.0 precise=true"),
                "{text}"
            );
            assert!(
                !text.contains("EVENT keydown w "),
                "deferred cleanup typed a key: {text}"
            );
            break;
        }
        assert!(Instant::now() < deadline, "native input log: {text}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
