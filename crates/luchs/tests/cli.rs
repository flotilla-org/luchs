mod common;

use std::{
    io::{BufRead, BufReader, Read},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use clap::Parser;
use common::{Want, connect_tracks};
use jackstay::input::Mode;
use luchs::cli::Cli;

#[test]
fn host_arguments_and_limits() {
    let cli = Cli::try_parse_from([
        "luchs",
        "--renderer=native-webview",
        "--size=1568x512",
        "--watch",
        "--",
        "page.html",
    ])
    .unwrap();
    assert_eq!((cli.size.0, cli.size.1), (1568, 512));
    assert!(cli.watch);
    assert_eq!(cli.page, "page.html");
    for size in [
        "0x1",
        "800X600",
        "1x0",
        "4294967295x4294967295",
        "99999x99999",
    ] {
        assert!(Cli::try_parse_from(["luchs", "--size", size, "page.html"]).is_err());
    }
    let cli = Cli::try_parse_from(["luchs", "https://example.com"]).unwrap();
    assert!(cli.local_page().is_none());
}

// Unix-only: these check the orderly SIGTERM stop. The Windows CLI has no stop
// request yet (terminating it skips cleanup), see #43.
#[cfg(unix)]
#[test]
fn watch_console_environment_and_sigterm_cleanup_with_fake_helper() {
    sigterm_cleanup(false);
}

// Optional input must acknowledge cleanup without failing an orderly SIGTERM.
#[cfg(unix)]
#[test]
fn optional_controls_do_not_fail_sigterm_cleanup() {
    sigterm_cleanup(true);
}

#[cfg(unix)]
fn sigterm_cleanup(optional_controls: bool) {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("page.html");
    let log = dir.path().join("console");
    let pid = dir.path().join("helper-pid");
    std::fs::write(&page, "old page").unwrap();
    // Fake only the renderer subprocess boundary; the handshake and media are real.
    let helper = common::helper_script(
        dir.path(),
        &format!(
            r#"
with open('{}', 'w') as out: out.write(str(os.getpid()))
with open(os.environ['LUCHS_CONSOLE_LOG'], 'w') as out: out.write(' '.join(sys.argv[1:]) + '\n')
frame()
while True:
    try: cmd = command()
    except EOFError: break
    ack(cmd)
    with open(os.environ['LUCHS_CONSOLE_LOG'], 'a') as out: out.write(json.dumps(cmd) + '\n')
"#,
            pid.display()
        ),
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_luchs"))
        .arg("--helper")
        .arg(&helper)
        .args(["--size=1x1", "--watch"])
        .arg(&page)
        .env("LUCHS_CONSOLE_LOG", &log)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut endpoint = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut endpoint)
        .unwrap();
    if endpoint.trim().is_empty() {
        let output = child.wait_with_output().unwrap();
        panic!(
            "CLI exited during setup: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pid.exists() || !log.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    // Force a later mtime on filesystems with coarse timestamp resolution.
    let file = std::fs::File::options().write(true).open(&page).unwrap();
    file.set_modified(std::time::SystemTime::now() + Duration::from_secs(2))
        .unwrap();
    while !std::fs::read_to_string(&log).unwrap().contains("reload") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let logged = std::fs::read_to_string(&log).unwrap();
    // SIGTERM must stop with a live consumer and reap the helper.
    let connected = connect_tracks(
        common::connect(endpoint.trim()),
        if optional_controls {
            Want::Optional
        } else {
            Want::No
        },
        Mode::Cooperative,
        Want::Optional,
    );
    if optional_controls {
        let input = connected.input.as_ref().unwrap();
        assert!(
            input
                .send(jackstay::input::Event::Text("native commit".into()))
                .is_ok()
        );
    }
    // SAFETY: the spawned luchs process is the sole conforming producer.
    let mut setup =
        unsafe { jackstay::acquisition::socket::CpuSetupClient::from_stream(connected.media) };
    let consumer = setup.attach(1).unwrap();
    // SAFETY: the test owns this unreaped child process.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    // A conforming consumer retires its mappings when the source stops.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !matches!(
        consumer.acquire_latest(0).unwrap(),
        jackstay::acquisition::arena::AcquireOutcome::Closed
    ) {
        assert!(Instant::now() < deadline, "source did not stop media");
        std::thread::sleep(Duration::from_millis(5));
    }
    drop((consumer, setup, connected.input, connected.affordances));
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("luchs did not stop");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(status.success(), "{stderr}");
    assert!(!stderr.contains("input cleanup failed"));
    if optional_controls {
        assert!(std::fs::read_to_string(&log).unwrap().contains("cleanup"));
    }
    assert!(logged.contains("1 1 0 30"));
    assert!(logged.contains("\"type\": \"reload\""));
    assert!(common::endpoint_gone(endpoint.trim()));
    let helper_pid = std::fs::read_to_string(pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(common::gone(helper_pid));
}

// Helper EOF before a completed draw must stop without publishing or waiting for a consumer.
#[test]
fn immediate_helper_eof_without_consumer_never_publishes() {
    let dir = tempfile::tempdir().unwrap();
    // Fake only the renderer subprocess boundary. On Windows the helper exits
    // without ever opening its pipe.
    #[cfg(unix)]
    let helper = {
        use std::os::unix::fs::PermissionsExt;
        let helper = dir.path().join("helper");
        std::fs::write(&helper, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        helper
    };
    #[cfg(windows)]
    let helper = {
        let helper = dir.path().join("helper.cmd");
        std::fs::write(&helper, "@exit /b 0\r\n").unwrap();
        helper
    };
    let mut child = Command::new(env!("CARGO_BIN_EXE_luchs"))
        .arg("--helper")
        .arg(&helper)
        .args(["--size=1x1", "https://example.com"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("EOF shutdown waited for a consumer");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(!status.success(), "{stderr}");
    assert!(!stderr.contains("stopped after 1 frames"));
    let mut endpoint = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut endpoint)
        .unwrap();
    assert!(common::endpoint_gone(endpoint.trim()));
}

// A malformed helper frame must fail the CLI, remove its endpoint via Drop,
// and reap a helper still blocked on stdin; explicit Source::stop is bypassed.
#[test]
fn invalid_helper_record_drops_source_and_reaps_helper() {
    let dir = tempfile::tempdir().unwrap();
    let pid = dir.path().join("helper-pid");
    // Fake only the renderer subprocess boundary.
    let helper = common::helper_script(
        dir.path(),
        &format!(
            "{}{}time.sleep(60)\n",
            common::record_pid(&pid),
            common::raw_write(&[1, 0, 0, 0, 99])
        ),
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_luchs"))
        .arg("--helper")
        .arg(&helper)
        .args(["--size=1x1", "https://example.com"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("malformed-frame shutdown did not finish");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(!status.success(), "{stderr}");
    assert!(stderr.contains("unknown helper record tag"), "{stderr}");
    let mut endpoint = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut endpoint)
        .unwrap();

    assert!(
        common::endpoint_gone(endpoint.trim()),
        "endpoint leaked: {}",
        endpoint.trim()
    );
    let helper_pid = std::fs::read_to_string(pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(common::gone(helper_pid));
}

struct Process(std::process::Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn watch_retries_failed_reload_without_another_file_change() {
    watch_reload_outcome("failed");
}

#[test]
fn watch_retries_uncertain_reload_without_another_file_change() {
    watch_reload_outcome("uncertain");
}

#[test]
fn watch_unsupported_reload_stops_the_run() {
    watch_reload_outcome("unsupported");
}

fn watch_reload_outcome(first_outcome: &str) {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("page.html");
    let log = dir.path().join("reloads");
    std::fs::write(&page, "old page").unwrap();
    let helper = common::helper_script(
        dir.path(),
        &format!(
            r#"
log = r'{}'
with open(log, 'w') as out: out.write('ready\n')
frame()
first = command()
assert first['type'] == 'reload'
if '{}' == 'uncertain':
    time.sleep(1.15)
    ack(first)
else:
    ack(first, '{}')
second = command()
assert second['type'] == 'reload' and second['id'] != first['id']
if '{}' == 'failed':
    ack(second, 'failed')
    second = command()
ack(second)
with open(log, 'a') as out: out.write('recovered\n')
while True:
    try: ack(command())
    except EOFError: break
"#,
            log.display(),
            first_outcome,
            first_outcome,
            first_outcome
        ),
    );
    let mut child = Process(
        Command::new(env!("CARGO_BIN_EXE_luchs"))
            .arg("--helper")
            .arg(&helper)
            .args(["--size=1x1", "--watch"])
            .arg(&page)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut endpoint = String::new();
    BufReader::new(child.0.stdout.take().unwrap())
        .read_line(&mut endpoint)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !log.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::File::options()
        .write(true)
        .open(&page)
        .unwrap()
        .set_modified(std::time::SystemTime::now() + Duration::from_secs(2))
        .unwrap();
    if first_outcome != "unsupported" {
        while !std::fs::read_to_string(&log).unwrap().contains("recovered") {
            assert!(Instant::now() < deadline, "watch did not retry");
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "watch stopped on {first_outcome}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        common::stop(&mut child.0);
    }
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "CLI did not stop");
        std::thread::sleep(Duration::from_millis(10));
    };
    // Windows terminates rather than stops the CLI (see common::stop).
    if cfg!(unix) || first_outcome == "unsupported" {
        assert_eq!(status.success(), first_outcome != "unsupported");
    }
    let mut stderr = String::new();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let expected = match first_outcome {
        "failed" => "renderer reload: Failed",
        "uncertain" => "renderer reload: Uncertain",
        _ => "renderer reload: unsupported",
    };
    assert!(stderr.contains(expected), "{stderr}");
    assert_eq!(
        stderr.matches(expected).count(),
        1,
        "duplicate retry diagnostics: {stderr}"
    );
    assert!(common::endpoint_gone(endpoint.trim()));
}

#[test]
fn exhausted_snapshot_retry_diagnostic_reaches_cli() {
    let dir = tempfile::tempdir().unwrap();
    let helper = common::helper_script(
        dir.path(),
        r#"
cmd = raw_command()
assert cmd['type'] == 'draw'
ack(cmd, 'failed', detail='snapshot failed after 3 consecutive attempts: unexpected pixel size')
"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_luchs"))
        .arg("--helper")
        .arg(helper)
        .args(["--size=1x1", "https://example.com"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("renderer capture failed: snapshot failed after 3 consecutive attempts: unexpected pixel size"), "{stderr}");
}

#[test]
fn helper_crash_during_input_preserves_renderer_error_and_removes_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let helper = common::helper_script(
        dir.path(),
        "frame()\ncmd=command()\nassert cmd['type']=='key_down'\nsys.exit(23)\n",
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_luchs"))
        .arg("--helper")
        .arg(&helper)
        .args(["--size=1x1", "https://example.com"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut endpoint = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut endpoint)
        .unwrap();
    if endpoint.trim().is_empty() {
        let output = child.wait_with_output().unwrap();
        panic!(
            "CLI exited during setup: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let connected = connect_tracks(
        common::connect(endpoint.trim()),
        Want::Required,
        Mode::Physical,
        Want::No,
    );
    let input = connected.input.as_ref().unwrap();
    input
        .send(jackstay::input::Event::Key {
            press: 1,
            action: jackstay::input::Action::Down,
            key: jackstay::input::Key::Physical("KeyA".into()),
            modifiers: 0,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("helper crash did not stop the CLI");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(
        !status.success(),
        "a renderer crash must not be reported as an orderly stop"
    );
    assert!(
        stderr.contains("renderer exited") && stderr.contains("23"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("input cleanup failed"),
        "cleanup masked the renderer error: {stderr}"
    );
    assert!(common::endpoint_gone(endpoint.trim()));
}
