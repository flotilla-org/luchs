mod common;

use luchs::{
    helper::{CommandOutcome, Helper, MAX_PENDING_COMMANDS},
    protocol::{self, Record, read_record},
};
use serde_json::json;
use std::{
    io,
    process::Command,
    sync::mpsc::{self, RecvTimeoutError},
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(3);

fn fake(script: &str) -> Helper {
    Helper::spawn(Command::new("/bin/sh").args(["-c", script])).unwrap()
}

fn python(script: &str) -> Helper {
    Helper::spawn(
        Command::new("python3").args(["-c", &format!("{}{script}", common::PYTHON_PROTOCOL)]),
    )
    .unwrap()
}

#[test]
fn fake_helper_streams_split_records_binary_pixels_and_resizes() {
    let first = common::frame(1, &[0, 10, 255, 1]);
    let second = common::frame(2, b"12345678");
    let mut helper = fake(&format!(
        "{}; {}; {}",
        common::printf(&first[..2]),
        common::printf(&first[2..]),
        common::printf(&second)
    ));
    let first = helper.receive(TIMEOUT).unwrap().unwrap();
    assert_eq!(first.pixels, [0, 10, 255, 1]);
    let second = helper.receive(TIMEOUT).unwrap().unwrap();
    assert_eq!(second.header.width, 2);
    assert_eq!(second.pixels, b"12345678");
    assert!(matches!(
        helper.receive(TIMEOUT),
        Err(RecvTimeoutError::Disconnected)
    ));
    helper.finish().unwrap();
}

#[test]
fn per_id_acks_can_arrive_out_of_order_after_frames() {
    let mut helper = python(
        r#"
a = command()
b = command()
assert a['type'] == 'reload' and b['type'] == 'ping'
# Fill the frame mailbox. It must never block control dispatch.
for _ in range(10): frame()
ack(b, 'unsupported')
control(3, {'url': 'test'})
ack(a)
"#,
    );
    let a = helper.send_command("reload", TIMEOUT).unwrap();
    let b = helper.send_command("ping", TIMEOUT).unwrap();
    assert_ne!(a.id(), b.id());
    assert_eq!(a.wait(), CommandOutcome::Executed);
    assert_eq!(b.wait(), CommandOutcome::Unsupported);
    assert_eq!(helper.receive(TIMEOUT).unwrap().unwrap().pixels, b"rgba");
    helper.finish().unwrap();
}

#[test]
fn timeout_and_late_ack_never_execute_another_command() {
    let mut helper = python(
        r#"
a = command()
time.sleep(0.15)
ack(a)
b = command()
ack(b, 'unsupported')
"#,
    );
    assert_eq!(
        helper.command("ping", Duration::from_millis(50)),
        CommandOutcome::Uncertain
    );
    assert_eq!(
        helper.command("future-input", TIMEOUT),
        CommandOutcome::Unsupported
    );
    helper.finish().unwrap();
    assert_eq!(
        CommandOutcome::Uncertain.execution_outcome(),
        jackstay::input::Outcome::Uncertain
    );
}

#[test]
fn waiting_after_deadline_does_not_accept_a_late_ack() {
    let mut helper = python("command_record = command()\ntime.sleep(0.15)\nack(command_record)\n");
    let pending = helper
        .send_command("ping", Duration::from_millis(50))
        .unwrap();
    helper.finish().unwrap();
    assert_eq!(pending.wait(), CommandOutcome::Uncertain);
}

#[test]
fn unknown_command_and_failed_command_preserve_outcomes() {
    let mut helper = python(
        r#"
a = command()
assert a['type'] == 'future-command'
ack(a, 'unsupported')
ack(command(), 'failed', 'no view')
"#,
    );
    assert_eq!(
        helper.command("future-command", TIMEOUT),
        CommandOutcome::Unsupported
    );
    assert_eq!(
        helper.command("reload", TIMEOUT),
        CommandOutcome::Failed(Some("no view".into()))
    );
    helper.finish().unwrap();
}

#[test]
fn state_event_is_delivered_to_callback() {
    let (send, receive) = mpsc::channel();
    let script = common::printf(&common::control(
        3,
        json!({"url":"file:///page", "nested":{"revision":7}}),
    ));
    let mut helper = Helper::spawn_with_state(
        Command::new("/bin/sh").args(["-c", &script]),
        move |state| {
            send.send(state).unwrap();
        },
    )
    .unwrap();
    let state = receive.recv_timeout(TIMEOUT).unwrap();
    assert_eq!(state["nested"]["revision"], 7);
    helper.finish().unwrap();
}

#[test]
fn invalid_records_are_rejected_before_reading_payloads() {
    let header = json!({"format":"rgba8", "width":1, "height":1, "stride":4, "len":4});
    let mut cases = vec![
        vec![0, 0, 0, 0],
        vec![1],
        common::envelope(&[99]),
        common::envelope(&[1, 0, 0, 0, 0]),
        common::control(2, json!({"id":1,"outcome":"imagined"})),
        common::control(2, json!({"id":-1,"outcome":"executed"})),
        common::control(3, json!([])),
        common::envelope(&[2, b'{']),
    ];
    for key in ["format", "width", "height", "stride", "len"] {
        let mut invalid = header.clone();
        invalid[key] = match key {
            "format" => json!("bgra8"),
            "width" | "height" => json!(0),
            "stride" => json!(3),
            _ => json!(5),
        };
        let json = serde_json::to_vec(&invalid).unwrap();
        let mut body = vec![1];
        body.extend_from_slice(&(json.len() as u32).to_le_bytes());
        body.extend_from_slice(&json);
        body.extend_from_slice(b"rgba");
        cases.push(common::envelope(&body));
    }
    for bytes in cases {
        assert!(
            read_record(&mut bytes.as_slice()).is_err(),
            "accepted {bytes:?}"
        );
    }
    let bytes = common::frame(1, b"rgba");
    for end in 1..bytes.len() {
        assert!(read_record(&mut &bytes[..end]).is_err());
    }
    assert!(read_record(&mut &b""[..]).unwrap().is_none());
}

#[test]
fn oversized_record_control_and_frame_header_are_rejected() {
    for (len, tag) in [
        (u32::MAX, 1),
        ((protocol::MAX_CONTROL_BYTES + 1) as u32, 2),
        ((protocol::MAX_CONTROL_BYTES + 1) as u32, 3),
    ] {
        let mut bytes = len.to_le_bytes().to_vec();
        bytes.push(tag);
        assert_eq!(
            read_record(&mut bytes.as_slice()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
    let mut bytes = (5000u32).to_le_bytes().to_vec();
    bytes.push(1);
    bytes.extend_from_slice(&(4097u32).to_le_bytes());
    assert_eq!(
        read_record(&mut bytes.as_slice()).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let header = serde_json::to_vec(&json!({
        "format":"rgba8", "width":1, "height":1,
        "stride":protocol::MAX_FRAME_BYTES + 1, "len":protocol::MAX_FRAME_BYTES + 1,
    }))
    .unwrap();
    let mut bytes = ((5 + header.len() + protocol::MAX_FRAME_BYTES + 1) as u32)
        .to_le_bytes()
        .to_vec();
    bytes.push(1);
    bytes.extend_from_slice(&(header.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&header);
    assert_eq!(
        read_record(&mut bytes.as_slice()).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert!(protocol::encode_command(1, &"x".repeat(protocol::MAX_CONTROL_BYTES)).is_err());
}

#[test]
fn command_ids_preserve_the_full_u64_range() {
    let bytes = protocol::encode_command(u64::MAX, "ping").unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes[4..]).unwrap();
    assert_eq!(json["id"].as_u64(), Some(u64::MAX));
    let bytes = common::control(2, json!({"id":u64::MAX,"outcome":"executed"}));
    let Record::Ack(ack) = read_record(&mut bytes.as_slice()).unwrap().unwrap() else {
        panic!()
    };
    assert_eq!(ack.id, u64::MAX);
}

#[test]
fn malformed_record_stops_and_reaps_without_waiting_for_drop() {
    let dir = tempfile::tempdir().unwrap();
    let pid_path = dir.path().join("pid");
    let helper = fake(&format!(
        "echo $$ > '{}'; {}; exec sleep 60",
        pid_path.display(),
        common::printf(&common::envelope(&[99]))
    ));
    assert_eq!(
        helper.receive(TIMEOUT).unwrap().unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_reaped(&pid_path);
}

fn assert_reaped(path: &std::path::Path) {
    let pid: i32 = std::fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal zero only probes whether the test child still exists.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}

#[test]
fn dropping_a_blocked_helper_terminates_and_reaps_it() {
    let dir = tempfile::tempdir().unwrap();
    let pid_path = dir.path().join("pid");
    let helper = fake(&format!(
        "echo $$ > '{}'; exec sleep 60",
        pid_path.display()
    ));
    let deadline = Instant::now() + TIMEOUT;
    while !pid_path.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(helper);
    assert_reaped(&pid_path);
}

#[test]
fn helper_exit_disconnects_pending_acks_as_uncertain() {
    let mut helper = python("command()\nsys.exit(7)\n");
    assert_eq!(helper.command("ping", TIMEOUT), CommandOutcome::Uncertain);
    assert!(helper.finish().unwrap_err().to_string().contains('7'));
}

#[test]
fn pending_slots_and_blocked_command_writes_are_bounded() {
    let mut helper = python("for _ in range(64): command()\ntime.sleep(60)\n");
    let pending: Vec<_> = (0..MAX_PENDING_COMMANDS)
        .map(|_| helper.send_command("ping", TIMEOUT).unwrap())
        .collect();
    assert_eq!(
        helper.send_command("ping", TIMEOUT).err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(pending);
    let start = Instant::now();
    assert_eq!(
        helper.command(&"x".repeat(120 * 1024), Duration::from_millis(100)),
        CommandOutcome::Uncertain
    );
    assert!(start.elapsed() < Duration::from_secs(1));
}
