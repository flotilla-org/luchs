mod common;
use jackstay::{
    acquisition::{arena::AcquireOutcome, socket::CpuSetupClient},
    affordances::{Presentation, Size},
    bootstrap::{ChannelRequest, InputRequest, connect_v2},
    input::{Event, Mode},
    local::Stream,
};
use luchs::capture::Viewport;
use std::{
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
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
    let deadline = Instant::now() + Duration::from_secs(8);
    while !check() {
        assert!(Instant::now() < deadline, "presentation timed out");
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
fn sizes_round_clamp_at_scale_and_restore_cli_default() {
    for (w, h, scale) in [
        (800.4, 600.6, 1.5),
        (1e12, 1e12, 2.),
        (f64::MAX, f64::MAX, 2.),
        (1e12, 1., 1.),
        (1., 1e12, 2.),
        (4096., 4096., 1.),
        (800., 600., 0.01),
    ] {
        let hint = Presentation {
            preferred_size: Some(Size {
                width: w,
                height: h,
            }),
            scale,
            ..Default::default()
        };
        let viewport = Viewport::resolve((320, 240), &hint, 1.);
        luchs::protocol::validate_size(viewport.pixels.0, viewport.pixels.1).unwrap();
        assert!(viewport.logical.0 > 0 && viewport.logical.1 > 0);
        assert_eq!(viewport.scale, scale);
    }
    let rounded = Viewport::resolve(
        (320, 240),
        &Presentation {
            preferred_size: Some(Size {
                width: 800.4,
                height: 600.6,
            }),
            scale: 1.5,
            ..Default::default()
        },
        1.,
    );
    assert_eq!(rounded.logical, (800, 601));
    assert_eq!(rounded.pixels, (1200, 902));
    let default = Viewport::resolve((320, 240), &Presentation::default(), 2.);
    assert_eq!(default.logical, (320, 240));
    assert_eq!(default.pixels, (320, 240));
}

#[test]
fn fake_host_resize_geometry_focus_bursts_and_withdrawal() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("helper");
    let log = dir.path().join("commands");
    std::fs::write(
        &script,
        format!(
            "#!/usr/bin/env python3\n{}\n{}",
            common::PYTHON_PROTOCOL,
            r#"
width, height, scale, fresh = 32, 24, 1, True
while True:
    try: cmd = raw_command()
    except EOFError: break
    with open(os.environ['PRESENTATION_LOG'], 'a') as log:
        log.write(json.dumps(cmd) + '\n')
    if cmd['type'] == 'resize':
        width, height, scale, fresh = cmd['width'], cmd['height'], cmd['scale'], True
        ack(cmd)
    elif cmd['type'] == 'draw':
        assert cmd['width'] == round(width * scale) and cmd['height'] == round(height * scale)
        ack(cmd, capture={'published':fresh, 'snapshot_ns':1, 'publish_ns':1})
        fresh = False
    else: ack(cmd)
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_luchs"))
            .args(["--size=32x24", "--helper"])
            .arg(&script)
            .arg("https://example.com")
            .env("PRESENTATION_LOG", &log)
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
    wait(|| {
        matches!(
            consumer.acquire_latest(0).unwrap(),
            AcquireOutcome::Frame(_)
        )
    });
    let hint = Presentation {
        preferred_size: Some(Size {
            width: 80.,
            height: 60.,
        }),
        scale: 2.,
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
        || matches!(consumer.acquire_latest(0).unwrap(), AcquireOutcome::Frame(f) if f.descriptor().width == 160 && f.descriptor().height == 120),
    );
    wait(|| input.welcome().config.geometry.width == 80.);
    assert_eq!(input.welcome().config.geometry.height, 60.);
    assert_eq!(input.welcome().config.geometry.revision, 2);
    assert!(
        events(&log)
            .iter()
            .any(|v| v["type"] == "resize" && v["width"] == 80 && v["scale"] == 2.)
    );
    // Focus changes alone never synthesize input cleanup or revoke the controller.
    let cleanups = events(&log)
        .iter()
        .filter(|v| v["type"] == "cleanup")
        .count();
    host.publish(Presentation {
        focused: false,
        ..hint.clone()
    })
    .unwrap();
    wait(|| {
        events(&log)
            .iter()
            .any(|v| v["type"] == "focus" && v["focused"] == false)
    });
    assert_eq!(
        events(&log)
            .iter()
            .filter(|v| v["type"] == "cleanup")
            .count(),
        cleanups
    );
    input.send(Event::Text("still admitted".into())).unwrap();
    wait(|| events(&log).iter().any(|v| v["type"] == "text"));
    // Scale alone changes pixels without advancing logical input geometry.
    host.publish(Presentation {
        scale: 1.,
        focused: false,
        ..hint.clone()
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
        || matches!(consumer.acquire_latest(0).unwrap(), AcquireOutcome::Frame(f) if f.descriptor().width == 80),
    );
    assert_eq!(input.welcome().config.geometry.revision, 2);
    // Pace the burst below the debounce interval; only its last size is applied.
    let resizes = events(&log)
        .iter()
        .filter(|v| v["type"] == "resize")
        .count();
    for width in [90., 100., 110.] {
        host.publish(Presentation {
            preferred_size: Some(Size { width, height: 60. }),
            scale: 1.,
            focused: false,
            ..Default::default()
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(15));
    }
    wait(|| {
        events(&log)
            .iter()
            .any(|v| v["type"] == "resize" && v["width"] == 110)
    });
    assert_eq!(
        events(&log)
            .iter()
            .filter(|v| v["type"] == "resize")
            .count(),
        resizes + 1
    );
    wait(|| {
        matches!(
            consumer.acquire_latest(0).unwrap(),
            AcquireOutcome::Reconfiguration
        )
    });
    setup.install_configuration(&mut consumer).unwrap();
    wait(
        || matches!(consumer.acquire_latest(0).unwrap(), AcquireOutcome::Frame(f) if f.descriptor().width == 110),
    );
    host.publish(Presentation {
        preferred_size: Some(Size {
            width: 110.,
            height: 60.,
        }),
        visible: false,
        focused: true,
        scale: 2.,
    })
    .unwrap();
    wait(|| {
        events(&log)
            .iter()
            .any(|v| v["type"] == "presentation" && v["visible"] == false)
    });
    // Withdrawal restores all defaults, including the original CLI viewport.
    host.withdraw().unwrap();
    wait(|| {
        events(&log)
            .iter()
            .any(|v| v["type"] == "resize" && v["width"] == 32 && v["height"] == 24)
    });
    wait(|| {
        matches!(
            consumer.acquire_latest(0).unwrap(),
            AcquireOutcome::Reconfiguration
        )
    });
    setup.install_configuration(&mut consumer).unwrap();
    wait(
        || matches!(consumer.acquire_latest(0).unwrap(), AcquireOutcome::Frame(f) if f.descriptor().width == 32 && f.descriptor().height == 24),
    );
    wait(|| input.welcome().config.geometry.width == 32.);
    let last = events(&log)
        .into_iter()
        .rfind(|v| v["type"] == "presentation")
        .unwrap();
    let focus = events(&log)
        .into_iter()
        .rfind(|v| v["type"] == "focus")
        .unwrap();
    assert_eq!(focus["focused"], false);
    assert_eq!(last["visible"], true);
    assert_eq!(last["scale"], 1.);
    drop((consumer, setup, input, host));
    unsafe {
        libc::kill(process.0.id() as i32, libc::SIGTERM);
    }
    wait(|| process.0.try_wait().unwrap().is_some());
}
