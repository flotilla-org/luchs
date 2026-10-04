mod common;

use jackstay::{
    acquisition::{arena::AcquireOutcome, socket::CpuSetupClient},
    affordances::Presentation,
    bootstrap::{ChannelRequest, InputRequest, connect_v2},
    input::Mode,
    local::Stream,
};
use luchs::capture::{CapturePolicy, IDLE_AFTER, IDLE_INTERVAL};
use std::{
    io::{BufRead, BufReader, Read},
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn exact_idle_threshold_and_command_wake() {
    let now = Instant::now();
    let mut policy = CapturePolicy::new(30, now);
    policy.completed(false, now + IDLE_AFTER);
    assert!(!policy.due(now + IDLE_AFTER + IDLE_INTERVAL - Duration::from_nanos(1)));
    assert_eq!(policy.wait(now + IDLE_AFTER), IDLE_INTERVAL);
    assert!(policy.due(now + IDLE_AFTER + IDLE_INTERVAL));
    policy.wake(now + IDLE_AFTER);
    assert!(policy.due(now + IDLE_AFTER));
    policy.presentation(false, 2., now);
    assert!(!policy.due(now + Duration::from_secs(10)));
    policy.presentation(true, 2., now);
    assert!(policy.due(now));
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn wait(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(6);
    while !check() {
        assert!(Instant::now() < deadline, "capture policy timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn events(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn socketpair_fake_checks_idle_wakes_scale_visibility_and_unchanged_publications() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("helper");
    let log = dir.path().join("commands");
    let page = dir.path().join("page.html");
    std::fs::write(&page, "fixture").unwrap();
    std::fs::write(&script, format!("#!/usr/bin/env python3\n{}\n{}", common::PYTHON_PROTOCOL, r#"
import select
scale, visible, fresh, revision = 1, True, True, 1
while True:
    if os.path.exists(os.environ['CAPTURE_WAKE']):
        os.unlink(os.environ['CAPTURE_WAKE'])
        with open(os.environ['CAPTURE_LOG'], 'a') as log:
            log.write(json.dumps({'type':'page-change','time':time.monotonic()}) + '\n')
        control(3, {'capture_changed':True})
        revision += 1
        fresh = True
    if not select.select([transport], [], [], .01)[0]: continue
    try: cmd = raw_command()
    except EOFError: break
    with open(os.environ['CAPTURE_LOG'], 'a') as log:
        log.write(json.dumps({'type':cmd['type'], 'time':time.monotonic(), **cmd}) + '\n')
    if cmd['type'] == 'capture':
        assert visible
        size = round(scale)
        pixels = bytes([revision, 0, 0, 255]) * size * size
        header = json.dumps({'format':'bgra8','width':size,'height':size,'stride':size*4,'len':len(pixels)}).encode()
        body = b'\x01' + struct.pack('<I',len(header)) + header + pixels
        # Deliberately repeat pixels, too: Rust must not republish a bad
        # renderer's duplicates even when its capture report says unchanged.
        wire.write(struct.pack('<I',len(body)) + body)
        ack(cmd, capture={'published':fresh,'snapshot_ns':1000,'publish_ns':100 if fresh else 0})
        fresh = False
    elif cmd['type'] == 'presentation':
        scale, visible, fresh = cmd['scale'], cmd['visible'], True
        ack(cmd)
    elif cmd['type'] == 'reload':
        revision += 1
        fresh = True
        ack(cmd)
    elif cmd['type'] == 'ping':
        ack(cmd)
        control(3, {'capture_changed': True})
    else: ack(cmd, 'unsupported')
"#)).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_luchs"))
            .args(["--size=1x1", "--watch", "--stats"])
            .arg("--helper")
            .arg(&script)
            .arg(&page)
            .env("CAPTURE_LOG", &log)
            .env("CAPTURE_WAKE", dir.path().join("wake"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut path = String::new();
    BufReader::new(process.0.stdout.take().unwrap())
        .read_line(&mut path)
        .unwrap();
    let connected = connect_v2(
        Stream::connect(path.trim()).unwrap(),
        InputRequest::Optional(Mode::Cooperative),
        ChannelRequest::Required,
    )
    .unwrap();
    let hints = connected.affordances.unwrap();
    let input = connected.input.unwrap();
    let mut setup = unsafe { CpuSetupClient::from_stream(connected.media) };
    let mut consumer = setup.attach(1).unwrap();
    let mut sequence = 0;
    wait(|| {
        if let AcquireOutcome::Frame(frame) = consumer.acquire_latest(0).unwrap() {
            sequence = frame.descriptor().sequence;
            true
        } else {
            false
        }
    });
    wait(|| {
        let captures: Vec<_> = events(&log)
            .into_iter()
            .filter(|v| v["type"] == "capture")
            .collect();
        captures
            .windows(2)
            .filter(|pair| {
                pair[1]["time"].as_f64().unwrap() - pair[0]["time"].as_f64().unwrap() >= 0.45
            })
            .count()
            >= 2
    });
    if let AcquireOutcome::Frame(frame) = consumer.acquire_latest(0).unwrap() {
        assert_eq!(
            frame.descriptor().sequence,
            sequence,
            "unchanged frames were republished"
        );
    }
    std::fs::File::options()
        .write(true)
        .open(&page)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(2))
        .unwrap();
    wait(|| events(&log).iter().any(|v| v["type"] == "reload"));
    wait(|| {
        let log = events(&log);
        let Some(index) = log.iter().position(|v| v["type"] == "reload") else {
            return false;
        };
        let Some(next) = log[index + 1..].iter().find(|v| v["type"] == "capture") else {
            return false;
        };
        assert!(
            next["time"].as_f64().unwrap() - log[index]["time"].as_f64().unwrap() < 0.2,
            "command did not wake capture"
        );
        true
    });
    // A page signal also wakes the same scheduler without a host command.
    std::thread::sleep(Duration::from_millis(1300));
    std::fs::write(dir.path().join("wake"), "changed").unwrap();
    wait(|| {
        let log = events(&log);
        let Some(index) = log.iter().position(|v| v["type"] == "page-change") else {
            return false;
        };
        let Some(next) = log[index + 1..].iter().find(|v| v["type"] == "capture") else {
            return false;
        };
        assert!(next["time"].as_f64().unwrap() - log[index]["time"].as_f64().unwrap() < 0.2);
        true
    });
    // This traverses the toolkit's real affordance callback, not a helper-only command.
    hints
        .publish(Presentation {
            visible: false,
            scale: 1e9,
            ..Default::default()
        })
        .unwrap();
    wait(|| {
        events(&log)
            .iter()
            .any(|v| v["type"] == "presentation" && v["visible"] == false)
    });
    let hint = events(&log)
        .into_iter()
        .find(|v| v["type"] == "presentation" && v["visible"] == false)
        .unwrap();
    assert_eq!(
        hint["scale"], 1.,
        "invalid scale must preserve previous scale while hiding"
    );
    std::thread::sleep(Duration::from_millis(100));
    let count = events(&log).len();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(events(&log).len(), count, "hidden page kept snapshotting");
    hints
        .publish(Presentation {
            visible: true,
            scale: 2.,
            ..Default::default()
        })
        .unwrap();
    wait(|| {
        matches!(
            consumer.acquire_latest(0).unwrap(),
            AcquireOutcome::Reconfiguration
        )
    });
    setup.install_configuration(&mut consumer).unwrap();
    wait(
        || matches!(consumer.acquire_latest(0).unwrap(), AcquireOutcome::Frame(f) if f.descriptor().width == 2 && f.descriptor().height == 2),
    );
    let log = events(&log);
    let index = log
        .iter()
        .rposition(|v| v["type"] == "presentation" && v["visible"] == true)
        .unwrap();
    let capture = log[index + 1..]
        .iter()
        .find(|v| v["type"] == "capture")
        .unwrap();
    assert!(capture["time"].as_f64().unwrap() - log[index]["time"].as_f64().unwrap() < 0.2);
    assert_eq!(input.welcome().config.geometry.width, 1.);
    assert_eq!(input.welcome().config.geometry.height, 1.);
    drop((consumer, setup, hints, input));
    unsafe {
        libc::kill(process.0.id() as i32, libc::SIGTERM);
    }
    wait(|| process.0.try_wait().unwrap().is_some());
    let mut stderr = String::new();
    process
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.contains("stats snapshots="), "{stderr}");
    assert!(stderr.contains("published=4 "), "{stderr}");
}
