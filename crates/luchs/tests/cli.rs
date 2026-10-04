mod common;

use std::{
    io::{BufRead, BufReader, Read},
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use clap::Parser;
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

#[test]
fn watch_console_environment_and_sigterm_cleanup_with_fake_helper() {
    sigterm_cleanup(false);
}

// Optional input must not make an orderly SIGTERM fail or send helper commands.
#[test]
fn optional_controls_do_not_fail_sigterm_cleanup() {
    sigterm_cleanup(true);
}

fn sigterm_cleanup(optional_controls: bool) {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("page.html");
    let helper = dir.path().join("helper");
    let log = dir.path().join("console");
    let pid = dir.path().join("helper-pid");
    std::fs::write(&page, "old page").unwrap();
    // Fake only the renderer subprocess boundary; bootstrap and media are real.
    std::fs::write(
        &helper,
        format!(
            "#!/usr/bin/env python3\n{}\n{}",
            common::PYTHON_PROTOCOL,
            format_args!(
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
            )
        ),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
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
    // SIGTERM must stop with a live v2 consumer and reap the helper.
    let connected = jackstay::bootstrap::connect_v2(
        jackstay::local::Stream::connect(endpoint.trim()).unwrap(),
        if optional_controls {
            jackstay::bootstrap::InputRequest::Optional(jackstay::input::Mode::Cooperative)
        } else {
            jackstay::bootstrap::InputRequest::None
        },
        jackstay::bootstrap::ChannelRequest::Optional,
    )
    .unwrap();
    if optional_controls {
        let input = connected.input.as_ref().unwrap();
        assert_eq!(
            input.send(jackstay::input::Event::Text("not executed".into())),
            Err(jackstay::input::Error::Unsupported)
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
    assert_eq!(
        stderr.contains("shutdown: input cleanup failed"),
        optional_controls
    );
    assert!(
        !std::fs::read_to_string(&log)
            .unwrap()
            .contains("not executed")
    );
    assert!(logged.contains("1 1 0 30"));
    assert!(logged.contains("\"type\": \"reload\""));
    assert!(!std::path::Path::new(endpoint.trim()).exists());
    let helper_pid: i32 = std::fs::read_to_string(pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal zero is a liveness probe only.
    assert_eq!(unsafe { libc::kill(helper_pid, 0) }, -1);
}

// Immediate helper EOF must flush its final frame and stop without any consumer.
#[test]
fn immediate_helper_eof_without_consumer_stops_successfully() {
    let dir = tempfile::tempdir().unwrap();
    let helper = dir.path().join("helper");
    // Fake only the renderer subprocess boundary.
    std::fs::write(
        &helper,
        format!(
            "#!/bin/sh\n{}\n",
            common::printf(&common::frame(1, b"rgba"))
        ),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
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
    assert!(status.success(), "{stderr}");
    assert!(stderr.contains("stopped after 1 frames"));
    let mut endpoint = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut endpoint)
        .unwrap();
    assert!(!std::path::Path::new(endpoint.trim()).exists());
}

// A malformed helper frame must fail the CLI, remove its endpoint via Drop,
// and reap a helper still blocked on stdin; explicit Source::stop is bypassed.
#[test]
fn invalid_helper_frame_drops_source_and_reaps_helper() {
    let dir = tempfile::tempdir().unwrap();
    let helper = dir.path().join("helper");
    let pid = dir.path().join("helper-pid");
    // Fake only the renderer subprocess boundary.
    std::fs::write(
        &helper,
        format!(
            r#"#!/bin/sh
echo $$ > '{}'
printf '\001\000\000\000\143' >&$LUCHS_HELPER_FD
exec sleep 60
"#,
            pid.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
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
    assert!(!endpoint.trim().is_empty());
    assert!(
        !std::path::Path::new(endpoint.trim()).exists(),
        "endpoint leaked: {}",
        endpoint.trim()
    );
    let helper_pid: i32 = std::fs::read_to_string(pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal zero only probes the helper whose PID the test recorded.
    assert_eq!(unsafe { libc::kill(helper_pid, 0) }, -1);
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
    let helper = dir.path().join("helper");
    let log = dir.path().join("reloads");
    std::fs::write(&page, "old page").unwrap();
    std::fs::write(
        &helper,
        format!(
            "#!/usr/bin/env python3\n{}\n{}",
            common::PYTHON_PROTOCOL,
            format_args!(
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
            )
        ),
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
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
        // SAFETY: the test owns the unreaped CLI child.
        unsafe {
            libc::kill(child.0.id() as i32, libc::SIGTERM);
        }
    }
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "CLI did not stop");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.success(), first_outcome != "unsupported");
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
    assert!(!std::path::Path::new(endpoint.trim()).exists());
}
