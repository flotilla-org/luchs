//! Real WebKit reflow and presentation focus, without changing input admission.
#![cfg(target_os = "macos")]
use jackstay::{
    acquisition::{arena::AcquireOutcome, socket::CpuSetupClient},
    affordances::{Presentation, Size},
    bootstrap::{ChannelRequest, InputRequest, connect_v2},
    input::Mode,
    local::Stream,
};
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn wait(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !check() {
        assert!(Instant::now() < deadline, "native presentation timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
#[ignore = "requires built Swift helper and live macOS desktop"]
fn native_resize_reflows_and_focus_controls_caret() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("console.log");
    let page =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/presentation.html");
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_luchs"))
            .args(["--size=800x600"])
            .arg(page)
            .env("LUCHS_CONSOLE_LOG", &log)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut path = String::new();
    BufReader::new(process.0.stdout.take().unwrap())
        .read_line(&mut path)
        .unwrap();
    let connected = connect_v2(
        Stream::connect(path.trim()).unwrap(),
        InputRequest::Required(Mode::Cooperative),
        ChannelRequest::Required,
    )
    .unwrap();
    let host = connected.affordances.unwrap();
    let input = connected.input.unwrap();
    let mut setup = unsafe { CpuSetupClient::from_stream(connected.media) };
    let mut consumer = setup.attach(1).unwrap();
    let contains = |text: &str| {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains(text)
    };
    wait(|| contains("800x600 columns=2 focused=false caret=none"));
    wait(|| {
        matches!(
            consumer.acquire_latest(0).unwrap(),
            AcquireOutcome::Frame(_)
        )
    });
    let hint = Presentation {
        preferred_size: Some(Size {
            width: 500.,
            height: 400.,
        }),
        scale: 1.5,
        focused: true,
        ..Default::default()
    };
    host.publish(hint.clone()).unwrap();
    wait(|| {
        matches!(
            consumer.acquire_latest(0).unwrap(),
            AcquireOutcome::Reconfiguration
        )
    });
    setup.install_configuration(&mut consumer).unwrap();
    wait(
        || matches!(consumer.acquire_latest(0).unwrap(), AcquireOutcome::Frame(f) if f.descriptor().width == 750 && f.descriptor().height == 600),
    );
    wait(|| contains("500x400 columns=1 focused=true caret=block"));
    wait(|| input.welcome().config.geometry.width == 500.);
    assert_eq!(input.welcome().config.geometry.revision, 2);
    host.publish(Presentation {
        focused: false,
        ..hint
    })
    .unwrap();
    wait(|| contains("500x400 columns=1 focused=false caret=none"));
    assert_eq!(input.welcome().config.geometry.revision, 2);
    host.withdraw().unwrap();
    wait(|| {
        matches!(
            consumer.acquire_latest(0).unwrap(),
            AcquireOutcome::Reconfiguration
        )
    });
    setup.install_configuration(&mut consumer).unwrap();
    wait(
        || matches!(consumer.acquire_latest(0).unwrap(), AcquireOutcome::Frame(f) if f.descriptor().width == 800 && f.descriptor().height == 600),
    );
    wait(|| contains("800x600 columns=2 focused=false caret=none"));
    wait(|| input.welcome().config.geometry.revision == 3);
    drop((consumer, setup, input, host));
    unsafe {
        libc::kill(process.0.id() as i32, libc::SIGTERM);
    }
    wait(|| process.0.try_wait().unwrap().is_some());
    assert!(process.0.wait().unwrap().success());
}
