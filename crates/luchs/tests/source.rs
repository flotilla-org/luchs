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
    bootstrap::{self, ChannelRequest, InputRequest},
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
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let connected =
        bootstrap::connect_v2(stream, InputRequest::None, ChannelRequest::None).unwrap();
    assert!(connected.input.is_none());
    assert!(connected.input_error.is_none());
    // SAFETY: the test source is a conforming sole producer. This process owns
    // the grant and never forwards it or forks with the received mappings.
    let mut setup = unsafe { CpuSetupClient::from_stream(connected.media) };
    let consumer = setup.attach(1).unwrap();
    (setup, consumer)
}

fn pixels(width: u32) -> Frame {
    Frame {
        header: Header {
            format: luchs::protocol::Format::Bgra8,
            width,
            height: 1,
            stride: width * 4,
            len: width as usize * 4,
        },
        pixels: vec![42; width as usize * 4],
    }
}

fn acquire(consumer: &ArenaConsumer) -> FrameLease {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let AcquireOutcome::Frame(frame) = consumer.acquire_latest(0).unwrap() {
            return frame;
        }
        assert!(Instant::now() < deadline, "expected a frame");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// Frames retain their BGRA/sync contract across replacement while old leases stay valid.
#[test]
fn private_bootstrap_source_publishes_bgra_and_reconfigures() {
    let (mut source, path) = source();
    // Private parent permissions protect the socket before inode chmod.
    let parent = std::path::Path::new(&path).parent().unwrap();
    assert_eq!(std::fs::metadata(parent).unwrap().mode() & 0o777, 0o700);
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
    source.publish(pixels(1)).unwrap();
    let old = acquire(&consumer);
    assert_eq!(old.bytes(), [42; 4]);
    assert_eq!(old.descriptor().pixel_format, 1);
    // Katzensteg rejects CPU frames without the copy-complete sync contract.
    assert_eq!(old.descriptor().sync_kind, 1);
    assert_eq!(old.descriptor().clock_domain, 2);
    assert_eq!(old.descriptor().damage_kind, 1);
    let old_generation = old.descriptor().config_generation;
    source.publish(pixels(2)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matches!(
        consumer.acquire_latest(0).unwrap(),
        AcquireOutcome::Reconfiguration
    ) {
        assert!(Instant::now() < deadline, "expected reconfiguration");
        std::thread::sleep(Duration::from_millis(5));
    }
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
    source.publish(pixels(2)).unwrap();
    assert_eq!(acquire(&consumer).bytes(), [42; 8]);
    // Shutdown closes even a connected consumer and removes the endpoint.
    drop((setup, consumer));
    source.stop().unwrap();
    assert!(!std::path::Path::new(&path).exists());
}

// An idle handshake cannot block media publication or ordered shutdown.
#[test]
fn stalled_bootstrap_does_not_block_frames_or_shutdown() {
    let (mut source, path) = source();
    let stalled = Stream::connect(&path).unwrap();
    let (setup, consumer) = connect(&path);
    source.publish(pixels(1)).unwrap();
    assert_eq!(acquire(&consumer).bytes(), [42; 4]);
    drop((setup, consumer));
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

// Killed consumers release held leases so repeated replacement consumers can attach.
#[test]
fn consumer_process_death_releases_reservation() {
    let (mut source, path) = source();
    let directory = tempfile::tempdir().unwrap();
    // More than max_incarnations, each dying with an outstanding frame lease.
    for index in 0..5 {
        source.publish(pixels(1)).unwrap();
        let ready = directory.path().join(index.to_string());
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "child_consumer", "--ignored"])
            .env("LUCHS_TEST_SOURCE", &path)
            .env("LUCHS_TEST_READY", &ready)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() && Instant::now() < deadline {
            source.publish(pixels(1)).unwrap();
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
    source.publish(pixels(1)).unwrap();
    assert_eq!(acquire(&consumer).bytes(), [42; 4]);
}

#[test]
fn all_typing_modes_advertise_native_families_and_preserve_media() {
    for mode in [
        jackstay::input::Mode::Physical,
        jackstay::input::Mode::SourceText,
        jackstay::input::Mode::Cooperative,
    ] {
        let (mut source, path) = source();
        let connected = bootstrap::connect_v2(
            Stream::connect(&path).unwrap(),
            InputRequest::Optional(mode),
            ChannelRequest::Optional,
        )
        .unwrap();
        assert!(connected.affordances.is_some());
        let input = connected.input.as_ref().unwrap();
        assert_eq!(
            input.welcome().config.capabilities,
            jackstay::input::CAP_ALL
        );
        assert_eq!(input.welcome().config.modes, 7);
        // SAFETY: this test owns a grant from its conforming sole producer.
        let mut setup = unsafe { CpuSetupClient::from_stream(connected.media) };
        let consumer = setup.attach(1).unwrap();
        source.publish(pixels(1)).unwrap();
        assert_eq!(acquire(&consumer).bytes(), [42; 4]);
        drop((consumer, setup, connected.input, connected.affordances));
        source.stop().unwrap();
    }
}
