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

fn request_frame(helper: &mut Helper) -> luchs::protocol::Frame {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        assert_eq!(helper.command("capture", TIMEOUT), CommandOutcome::Executed);
        match helper.receive(Duration::from_millis(100)) {
            Ok(frame) => return frame.unwrap(),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(error) => panic!("renderer stopped: {error}"),
        }
        assert!(Instant::now() < deadline, "no native frame");
    }
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
    let mut helper = Helper::spawn(
        Command::new(&renderer)
            .arg(&html)
            .args(["32", "32", "0", "15"]),
    )
    .unwrap();
    let initial = request_frame(&mut helper);
    assert_eq!(initial.header.width, 32);
    assert_eq!(&initial.pixels[..4], &[0, 0, 255, 255]);
    assert_eq!(helper.command("ping", TIMEOUT), CommandOutcome::Executed);
    assert_eq!(
        helper.command("mouse_down", TIMEOUT),
        CommandOutcome::Unsupported
    );
    page(&html, "blue");
    assert_eq!(helper.reload().unwrap(), CommandOutcome::Executed);
    let deadline = Instant::now() + TIMEOUT;
    std::thread::sleep(Duration::from_millis(200));
    loop {
        let pending = helper.send_command("capture", TIMEOUT).unwrap();
        let frame = helper.receive(TIMEOUT).unwrap().unwrap();
        assert_eq!(pending.wait(), CommandOutcome::Executed);
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
    use std::sync::mpsc::RecvTimeoutError;
    let dir = tempfile::tempdir().unwrap();
    let html = dir.path().join("page.html");
    page(&html, "red");
    let renderer =
        std::path::Path::new(env!("CARGO_BIN_EXE_luchs")).with_file_name("luchs-webview-capture");
    let mut helper = Helper::spawn(
        Command::new(renderer)
            .arg(html)
            .args(["32", "32", "0", "30"]),
    )
    .unwrap();
    assert_eq!(request_frame(&mut helper).header.width, 32);
    assert_eq!(helper.command("capture", TIMEOUT), CommandOutcome::Executed);
    assert!(matches!(
        helper.receive(Duration::from_millis(100)),
        Err(RecvTimeoutError::Timeout)
    ));
    assert_eq!(
        helper
            .send_json_command(
                json!({"type":"presentation", "scale":2., "visible":false}),
                TIMEOUT
            )
            .unwrap()
            .wait(),
        CommandOutcome::Executed
    );
    assert_eq!(helper.command("capture", TIMEOUT), CommandOutcome::Executed);
    assert!(matches!(
        helper.receive(Duration::from_millis(100)),
        Err(RecvTimeoutError::Timeout)
    ));
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
    let pending = helper.send_command("capture", TIMEOUT).unwrap();
    let frame = helper.receive(TIMEOUT).unwrap().unwrap();
    assert_eq!((frame.header.width, frame.header.height), (64, 64));
    assert_eq!(&frame.pixels[..4], &[0, 0, 255, 255]);
    assert_eq!(pending.wait(), CommandOutcome::Executed);
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
    let mut helper = Helper::spawn(
        Command::new(renderer)
            .arg(html)
            .args(["17", "11", "0", "30"]),
    )
    .unwrap();
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
        let frame = request_frame(&mut helper);
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
    let mut helper = Helper::spawn(
        Command::new(renderer)
            .arg(url)
            .args(["32", "32", "0", "30"]),
    )
    .unwrap();
    waiting.recv_timeout(TIMEOUT).unwrap();
    let start = Instant::now();
    assert_eq!(
        helper.command("capture", Duration::from_secs(1)),
        CommandOutcome::Executed
    );
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
