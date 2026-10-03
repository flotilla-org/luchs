use std::{
    os::unix::fs::MetadataExt,
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

use jackstay::{
    acquisition::{
        arena::{AcquireOutcome, ArenaConsumer, FrameLease},
        socket::CpuSetupClient,
    },
    bootstrap::{self, InputRequest},
    local::Stream,
};
use luchs::{
    protocol::{Frame, Header},
    source::Source,
};

fn source() -> (Source, String) {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    Source::bind(
        &format!(
            "luchs-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
        1,
        1,
    )
    .unwrap()
}

fn connect(path: &str) -> (CpuSetupClient, ArenaConsumer) {
    let stream = Stream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let connected = bootstrap::connect(
        stream,
        InputRequest::Optional(jackstay::input::Mode::Cooperative),
    )
    .unwrap();
    assert!(connected.input.is_none());
    assert_eq!(
        connected.input_error,
        Some(jackstay::input::Error::Unsupported)
    );
    // SAFETY: the test source is a conforming sole producer. This process owns
    // the grant and never forwards it or forks with the received mappings.
    let mut setup = unsafe { CpuSetupClient::from_stream(connected.media) };
    let consumer = setup.attach(1).unwrap();
    (setup, consumer)
}

fn pixels(width: u32) -> Frame {
    Frame {
        header: Header {
            format: "rgba8".into(),
            width,
            height: 1,
            stride: width * 4,
            len: width as usize * 4,
        },
        pixels: vec![42; width as usize * 4],
    }
}

fn acquire(consumer: &ArenaConsumer) -> FrameLease {
    match consumer.acquire_latest(0).unwrap() {
        AcquireOutcome::Frame(frame) => frame,
        _ => panic!("expected a frame"),
    }
}

#[test]
fn private_bootstrap_source_publishes_rgba_and_reconfigures() {
    let (mut source, path) = source();
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert!(
        Source::bind(
            path.rsplit('/').next().unwrap().trim_end_matches(".sock"),
            1,
            1
        )
        .is_err()
    );
    let (mut setup, mut consumer) = connect(&path);
    source.publish(&pixels(1)).unwrap();
    let old = acquire(&consumer);
    assert_eq!(old.bytes(), [42; 4]);
    assert_eq!(old.descriptor().pixel_format, 2);
    // Katzensteg rejects CPU frames without the copy-complete sync contract.
    assert_eq!(old.descriptor().sync_kind, 1);
    assert_eq!(old.descriptor().clock_domain, 2);
    assert_eq!(old.descriptor().damage_kind, 1);
    let old_generation = old.descriptor().config_generation;
    source.publish(&pixels(2)).unwrap();
    assert!(matches!(
        consumer.acquire_latest(0).unwrap(),
        AcquireOutcome::Reconfiguration
    ));
    setup.install_configuration(&mut consumer).unwrap();
    assert_eq!(old.bytes(), [42; 4]);
    drop(old);
    let new = acquire(&consumer);
    assert_eq!(new.bytes(), [42; 8]);
    assert_eq!(new.descriptor().width, 2);
    assert!(new.descriptor().config_generation > old_generation);
    drop(new);
    drop(consumer);
    drop(setup);
    let (setup, consumer) = connect(&path);
    source.publish(&pixels(2)).unwrap();
    assert_eq!(acquire(&consumer).bytes(), [42; 8]);
    // Shutdown closes even a connected consumer and removes the endpoint.
    drop(source);
    assert!(!std::path::Path::new(&path).exists());
    drop((setup, consumer));
}

#[test]
fn stalled_bootstrap_does_not_block_frames_or_shutdown() {
    let (mut source, path) = source();
    let stalled = Stream::connect(&path).unwrap();
    let (_setup, consumer) = connect(&path);
    source.publish(&pixels(1)).unwrap();
    assert_eq!(acquire(&consumer).bytes(), [42; 4]);
    let started = Instant::now();
    drop(source);
    assert!(started.elapsed() < Duration::from_secs(2));
    drop(stalled);
}

#[test]
#[ignore = "spawned by consumer_process_death_releases_reservation"]
fn child_consumer() {
    let (_setup, consumer) = connect(&std::env::var("LUCHS_TEST_SOURCE").unwrap());
    let _held = acquire(&consumer);
    std::fs::write(std::env::var("LUCHS_TEST_READY").unwrap(), "ready").unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[test]
fn consumer_process_death_releases_reservation() {
    let (mut source, path) = source();
    let directory = tempfile::tempdir().unwrap();
    // More than max_incarnations, each dying with an outstanding frame lease.
    for index in 0..5 {
        source.publish(&pixels(1)).unwrap();
        let ready = directory.path().join(index.to_string());
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_consumer", "--ignored"])
            .env("LUCHS_TEST_SOURCE", &path)
            .env("LUCHS_TEST_READY", &ready)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() && Instant::now() < deadline {
            source.publish(&pixels(1)).unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }
        let attached = ready.exists();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(attached, "child {index} did not attach");
        // Let the kernel death watcher run; subsequent publish polls cleanup.
        std::thread::sleep(Duration::from_millis(50));
    }
    let (_setup, consumer) = connect(&path);
    source.publish(&pixels(1)).unwrap();
    assert_eq!(acquire(&consumer).bytes(), [42; 4]);
}
