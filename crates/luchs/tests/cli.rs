use std::{
    io::{BufRead, BufReader},
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
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("page.html");
    let helper = dir.path().join("helper");
    let log = dir.path().join("console");
    let pid = dir.path().join("helper-pid");
    std::fs::write(&page, "old page").unwrap();
    std::fs::write(
        &helper,
        format!(
            r#"#!/bin/sh
echo $$ > '{}'
printf '%s\n' "$1 $2 $3 $4 $5" > "$LUCHS_CONSOLE_LOG"
printf 'LUCHS_RAW_FRAME {{"format":"rgba8","width":1,"height":1,"stride":4,"len":4}}\nrgba'
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$LUCHS_CONSOLE_LOG"
done
"#,
            pid.display()
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
    // SAFETY: the test owns this unreaped child process.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(3);
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
    assert!(status.success());
    assert!(logged.contains("1 1 0 30"));
    assert!(logged.contains("{\"type\":\"reload\"}"));
    assert!(!std::path::Path::new(endpoint.trim()).exists());
    let helper_pid: i32 = std::fs::read_to_string(pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // SAFETY: signal zero is a liveness probe only.
    assert_eq!(unsafe { libc::kill(helper_pid, 0) }, -1);
}
