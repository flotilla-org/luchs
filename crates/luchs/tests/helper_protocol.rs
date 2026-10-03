use std::{io, process::Command, sync::mpsc::RecvTimeoutError, time::Duration};

use luchs::{helper::Helper, protocol::read_frame};

fn fake(script: &str) -> Helper {
    Helper::spawn(Command::new("/bin/sh").args(["-c", script])).unwrap()
}

fn record(width: u32, height: u32, stride: u32, len: usize) -> String {
    format!(
        "LUCHS_RAW_FRAME {{\"format\":\"rgba8\",\"width\":{width},\"height\":{height},\"stride\":{stride},\"len\":{len}}}\n"
    )
}

#[test]
fn fake_helper_streams_split_headers_binary_pixels_and_resizes() {
    let mut helper = fake(&format!(
        "printf 'LUCHS_RAW_'; printf 'FRAME {}'; printf '\\000\\012\\377\\001'; printf '{}'; printf '12345678'",
        record(1, 1, 4, 4).trim_start_matches("LUCHS_RAW_FRAME "),
        record(2, 1, 8, 8)
    ));
    let first = helper.receive(Duration::from_secs(3)).unwrap().unwrap();
    assert_eq!(first.pixels, [0, 10, 255, 1]);
    let second = helper.receive(Duration::from_secs(3)).unwrap().unwrap();
    assert_eq!(second.header.width, 2);
    assert_eq!(second.pixels, b"12345678");
    assert!(matches!(
        helper.receive(Duration::from_secs(3)),
        Err(RecvTimeoutError::Disconnected)
    ));
    helper.finish().unwrap();
}

#[test]
fn fake_helper_receives_unchanged_reload_json_on_stdin() {
    let helper_script = format!(
        "IFS= read -r line; test \"$line\" = '{{\"type\":\"reload\"}}' || exit 9; printf '{}'; printf 'rgba'",
        record(1, 1, 4, 4)
    );
    let mut helper = fake(&helper_script);
    helper.reload().unwrap();
    assert_eq!(
        helper
            .receive(Duration::from_secs(3))
            .unwrap()
            .unwrap()
            .pixels,
        b"rgba"
    );
    helper.finish().unwrap();
}

#[test]
fn fake_helper_truncation_and_failure_are_reported() {
    let helper = fake(&format!("printf '{}'; printf 'xx'", record(1, 1, 4, 4)));
    assert_eq!(
        helper
            .receive(Duration::from_secs(3))
            .unwrap()
            .unwrap_err()
            .kind(),
        io::ErrorKind::UnexpectedEof
    );
    let mut failed = fake("exit 7");
    assert!(matches!(
        failed.receive(Duration::from_secs(3)),
        Err(RecvTimeoutError::Disconnected)
    ));
    assert!(failed.finish().unwrap_err().to_string().contains('7'));
}

#[test]
fn invalid_headers_are_rejected_before_reading_payloads() {
    let cases = [
        "wrong\n".to_string(),
        "LUCHS_RAW_FRAME {broken}\n".to_string(),
        "x".repeat(4097),
        record(1, 1, 4, 4).trim_end().into(),
        record(0, 1, 4, 4),
        record(1, 0, 4, 0),
        record(2, 1, 4, 4),
        record(1, 1, 4, 5),
        record(1, 1, 4, 4).replace("rgba8", "bgra8"),
        record(u32::MAX, u32::MAX, u32::MAX, usize::MAX),
        record(1, 1, 67_108_865, 67_108_865),
    ];
    for case in cases {
        assert!(read_frame(&mut case.as_bytes()).is_err(), "accepted {case}");
    }
}

#[test]
fn dropping_a_blocked_helper_terminates_and_reaps_it() {
    let dir = tempfile::tempdir().unwrap();
    let pid_path = dir.path().join("pid");
    let helper = fake(&format!(
        "echo $$ > '{}'; exec sleep 60",
        pid_path.display()
    ));
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while !pid_path.exists() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid: i32 = std::fs::read_to_string(pid_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    drop(helper);
    // SAFETY: signal zero only probes whether the test child still exists.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}
