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
    let initial = helper.receive(TIMEOUT).unwrap().unwrap();
    assert_eq!(initial.header.width, 32);
    assert_eq!(&initial.pixels[..4], &[255, 0, 0, 255]);
    assert_eq!(helper.command("ping", TIMEOUT), CommandOutcome::Executed);
    assert_eq!(
        helper.command("mouse_down", TIMEOUT),
        CommandOutcome::Unsupported
    );
    page(&html, "blue");
    assert_eq!(helper.reload().unwrap(), CommandOutcome::Executed);
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let frame = helper.receive(TIMEOUT).unwrap().unwrap();
        if frame.pixels[..4] == [0, 0, 255, 255] {
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
    for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
        if color[2] == 255 {
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
fn native_stdin_eof_exits_successfully() {
    use luchs::protocol::{Record, encode_command, read_record};
    use std::{io::Write, sync::mpsc};
    let dir = tempfile::tempdir().unwrap();
    let html = dir.path().join("page.html");
    page(&html, "red");
    let renderer =
        std::path::Path::new(env!("CARGO_BIN_EXE_luchs")).with_file_name("luchs-webview-capture");
    let mut process = Process(
        Command::new(renderer)
            .arg(html)
            .args(["32", "32", "0", "15"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let stdout = process.0.stdout.take().unwrap();
    let (send, receive) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        while let Some(record) = read_record(&mut stdout).unwrap() {
            if let Record::Ack(ack) = record {
                send.send(ack).unwrap();
            }
        }
    });
    process
        .0
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&encode_command(u64::MAX, "ping").unwrap())
        .unwrap();
    let ack = receive.recv_timeout(TIMEOUT).unwrap();
    assert_eq!(ack.id, u64::MAX);
    assert_eq!(ack.outcome, luchs::protocol::AckOutcome::Executed);
    process.0.stdin.take();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = process.0.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Swift helper did not exit on EOF"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    reader.join().unwrap();
}
