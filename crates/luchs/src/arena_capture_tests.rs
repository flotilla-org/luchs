use super::*;
use crate::{arena_capture::Draw, protocol::Ack};
use jackstay::{
    acquisition::{
        arena::{AcquireOutcome, ArenaConsumer},
        socket::CpuSetupClient,
    },
    bootstrap::{ChannelRequest, InputRequest, connect_v2},
    local::Stream,
};
use std::{process::Command, sync::atomic::AtomicU32};
#[path = "../tests/common/mod.rs"]
mod common;

const TIMEOUT: Duration = Duration::from_secs(3);
fn source() -> (Source, String) {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    Source::bind(
        &format!(
            "arena-draw-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
        1,
        1,
    )
    .unwrap()
}
fn helper(script: &str) -> Helper {
    Helper::spawn(
        Command::new("python3").args(["-c", &format!("{}{script}", common::PYTHON_PROTOCOL)]),
    )
    .unwrap()
}
fn reply(draw: &Draw) -> Ack {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(ack) = draw.poll() {
            return ack;
        }
        assert!(Instant::now() < deadline, "missing draw reply");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn connect(path: &str) -> (CpuSetupClient, ArenaConsumer) {
    let connected = connect_v2(
        Stream::connect(path).unwrap(),
        InputRequest::None,
        ChannelRequest::None,
    )
    .unwrap();
    // SAFETY: this test owns its conforming source and grant.
    let mut setup = unsafe { CpuSetupClient::from_stream(connected.media) };
    let consumer = setup.attach(2).unwrap();
    (setup, consumer)
}
fn free_slots(source: &Source) -> usize {
    source.source.with_arena(|arena| {
        let mut reservations = Vec::new();
        while let Some(slot) = arena.reserve().unwrap() {
            reservations.push(slot);
        }
        reservations.len()
    })
}
fn assert_reaped(helper: &mut Helper) {
    assert!(helper.ended());
    assert!(helper.finish().is_err());
}

#[test]
fn exported_slots_reach_consumers_byte_exact_and_resize_reexports() {
    let (mut source, path) = source();
    let helper = helper(
        r#"
previous = None
while True:
    cmd = raw_command()
    assert cmd['type'] == 'draw'
    if previous is not None: assert cmd['generation'] > previous
    previous = cmd['generation']
    pixels = bytes([0, 10, 255, 1]) * (cmd['width'] * cmd['height'])
    ack(cmd, capture={'published':True,'snapshot_ns':10,'publish_ns':10}, pixels=pixels)
"#,
    );
    source.setup_writer(&helper).unwrap();
    let (mut setup, mut consumer) = connect(&path);
    let draw = source.draw(&helper, 1, 1, TIMEOUT).unwrap().unwrap();
    let generation = draw.reservation.slot().generation;
    let ack = reply(&draw);
    source.complete_draw(&helper, draw, ack).unwrap();
    let AcquireOutcome::Frame(old) = consumer.acquire_latest(0).unwrap() else {
        panic!()
    };
    assert_eq!(old.bytes(), [0, 10, 255, 1]);
    let draw = source.draw(&helper, 3, 2, TIMEOUT).unwrap().unwrap();
    assert!(draw.reservation.slot().generation > generation);
    let ack = reply(&draw);
    source.complete_draw(&helper, draw, ack).unwrap();
    assert!(matches!(
        consumer.acquire_latest(0).unwrap(),
        AcquireOutcome::Reconfiguration
    ));
    setup.install_configuration(&mut consumer).unwrap();
    let AcquireOutcome::Frame(new) = consumer.acquire_latest(0).unwrap() else {
        panic!()
    };
    assert_eq!(new.bytes(), [0, 10, 255, 1].repeat(6));
    assert_eq!(
        (
            new.descriptor().width,
            new.descriptor().height,
            new.descriptor().stride
        ),
        (3, 2, 12)
    );
    assert_eq!(old.bytes(), [0, 10, 255, 1]);
    drop((old, new, consumer, setup));
    source.stop().unwrap();
}

#[test]
fn unchanged_and_discarded_draws_abandon_every_reserved_slot() {
    let (mut source, path) = source();
    let helper = helper(
        r#"
while True:
    cmd = raw_command()
    ack(cmd, capture={'published':False,'snapshot_ns':10,'publish_ns':0})
"#,
    );
    let (setup, consumer) = connect(&path);
    for _ in 0..20 {
        let draw = source.draw(&helper, 1, 1, TIMEOUT).unwrap().unwrap();
        let ack = reply(&draw);
        source.complete_draw(&helper, draw, ack).unwrap();
        assert_eq!(free_slots(&source), 8);
        assert!(matches!(
            consumer.acquire_latest(0).unwrap(),
            AcquireOutcome::Empty
        ));
    }
    drop((setup, consumer));
    source.stop().unwrap();
}

#[test]
fn timeout_reaps_helper_before_slot_reuse_and_never_publishes() {
    let (mut source, path) = source();
    let mut helper = helper("cmd = raw_command()\ntime.sleep(60)\n");
    let (setup, consumer) = connect(&path);
    let draw = source
        .draw(&helper, 1, 1, Duration::from_millis(30))
        .unwrap()
        .unwrap();
    while !draw.expired() {
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(draw);
    assert_reaped(&mut helper);
    assert_eq!(free_slots(&source), 8);
    assert!(matches!(
        consumer.acquire_latest(0).unwrap(),
        AcquireOutcome::Empty
    ));
    drop((setup, consumer));
}

#[test]
fn generation_slot_header_and_id_mismatch_never_commit() {
    for field in ["generation", "slot", "width", "id"] {
        let (mut source, path) = source();
        let mut helper = helper(
            "cmd = raw_command()\nack(cmd, capture={'published':True,'snapshot_ns':10,'publish_ns':10})\ntime.sleep(60)\n",
        );
        let (setup, consumer) = connect(&path);
        let draw = source.draw(&helper, 1, 1, TIMEOUT).unwrap().unwrap();
        let mut ack = reply(&draw);
        match field {
            "generation" => ack.generation = Some(99),
            "slot" => ack.slot = Some(99),
            "width" => ack.frame.as_mut().unwrap().width = 99,
            _ => ack.id = 99,
        }
        assert!(source.complete_draw(&helper, draw, ack).is_err());
        assert_reaped(&mut helper);
        assert_eq!(free_slots(&source), 8);
        assert!(matches!(
            consumer.acquire_latest(0).unwrap(),
            AcquireOutcome::Empty
        ));
        drop((setup, consumer));
    }
}

#[test]
fn helper_death_and_death_after_ack_never_commit() {
    for script in [
        "raw_command()\nsys.exit(7)\n",
        "cmd=raw_command()\nack(cmd, capture={'published':True,'snapshot_ns':10,'publish_ns':10})\nsys.exit(7)\n",
    ] {
        let (mut source, path) = source();
        let mut helper = helper(script);
        let (setup, consumer) = connect(&path);
        let draw = source.draw(&helper, 1, 1, TIMEOUT).unwrap().unwrap();
        let deadline = Instant::now() + TIMEOUT;
        while !helper.ended() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(ack) = draw.poll() {
            assert!(source.complete_draw(&helper, draw, ack).is_err());
        } else {
            drop(draw);
        }
        assert_reaped(&mut helper);
        assert_eq!(free_slots(&source), 8);
        assert!(matches!(
            consumer.acquire_latest(0).unwrap(),
            AcquireOutcome::Empty
        ));
        drop((setup, consumer));
        source.stop().unwrap();
    }
}

#[test]
fn paused_resize_releases_writer_and_retries_pending_configuration() {
    // Both allocations have 128 KiB of pixels, so only one fits the 192 KiB
    // budget on macOS or Linux. The consumer deliberately pins the old mapping.
    let (mut source, path) = Source::bind_page_with_budget(
        &format!("arena-budget-{}", std::process::id()),
        64,
        64,
        None,
        192 * 1024,
    )
    .unwrap();
    let helper = helper(
        "while True:\n    cmd=raw_command()\n    ack(cmd, capture={'published':True,'snapshot_ns':10,'publish_ns':10})\n",
    );
    source.setup_writer(&helper).unwrap();
    let (setup, consumer) = connect(&path);
    let draw = source.draw(&helper, 64, 64, TIMEOUT).unwrap().unwrap();
    let ack = reply(&draw);
    source.complete_draw(&helper, draw, ack).unwrap();
    // Change dimensions and stride without changing the payload byte count.
    let first = source.draw(&helper, 128, 32, TIMEOUT).unwrap();
    assert!(
        first.is_none(),
        "test budget must force a paused replacement"
    );
    assert!(source.mapping.is_none());
    assert!(source.pending_dimensions.is_some());
    assert!(source.draw(&helper, 128, 32, TIMEOUT).unwrap().is_none());
    drop((consumer, setup));
    let deadline = Instant::now() + TIMEOUT;
    let draw = loop {
        if let Some(draw) = source.draw(&helper, 128, 32, TIMEOUT).unwrap() {
            break draw;
        }
        assert!(
            Instant::now() < deadline,
            "writer export prevented resize recovery"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let ack = reply(&draw);
    source.complete_draw(&helper, draw, ack).unwrap();
    assert!(source.pending_dimensions.is_none());
    source.stop().unwrap();
}

#[test]
fn hiding_before_draw_completion_abandons_changed_pixels() {
    let (mut source, path) = source();
    let helper = helper(
        "while True:\n    cmd=raw_command()\n    ack(cmd, capture={'published':True,'snapshot_ns':10,'publish_ns':10})\n",
    );
    let (setup, consumer) = connect(&path);
    let draw = source.draw(&helper, 1, 1, TIMEOUT).unwrap().unwrap();
    let ack = reply(&draw);
    let report = source
        .finish_draw(&helper, draw, ack, false)
        .unwrap()
        .unwrap();
    assert!(!report.published);
    assert_eq!(free_slots(&source), 8);
    assert!(matches!(
        consumer.acquire_latest(0).unwrap(),
        AcquireOutcome::Empty
    ));
    drop((setup, consumer));
    source.stop().unwrap();
}

#[test]
fn stopping_with_live_writer_unmaps_it_and_keeps_helper_available_for_cleanup() {
    let (mut source, _) = source();
    let mut helper = helper(
        r#"
while True:
    cmd=raw_command()
    if cmd['type'] == 'draw': ack(cmd, capture={'published':True,'snapshot_ns':10,'publish_ns':10})
    else:
        assert cmd['type'] in ['cleanup', 'ping']
        assert allocation is None
        ack(cmd)
"#,
    );
    source.attach_input(&helper);
    let draw = source.draw(&helper, 1, 1, TIMEOUT).unwrap().unwrap();
    let ack = reply(&draw);
    source.complete_draw(&helper, draw, ack).unwrap();
    source.stop().unwrap();
    assert_eq!(helper.command("cleanup", TIMEOUT), CommandOutcome::Executed);
    assert_eq!(helper.command("ping", TIMEOUT), CommandOutcome::Executed);
}
