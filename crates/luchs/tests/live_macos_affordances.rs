//! Production WebKit + required v2 affordances. SDL gestures are recorded separately.
#![cfg(target_os = "macos")]
use jackstay::{
    acquisition::socket::CpuSetupClient,
    affordances::{Domain, Event, Host, Snapshot, Verb},
    bootstrap::{ChannelRequest, InputRequest, connect_v2},
    local::Stream,
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use url::Url;
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn wait(host: &Host, mut predicate: impl FnMut(&Snapshot) -> bool) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        while let Some(event) = host.poll() {
            if let Event::Snapshot(snapshot) = event {
                eprintln!("native affordance: {snapshot:?}");
                if predicate(&snapshot) {
                    return snapshot;
                }
            }
        }
        assert!(!host.finished(), "native affordances closed");
        assert!(Instant::now() < deadline, "native affordance timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn verb(host: &Host, domain: Domain, name: &str, body: Value) {
    host.send(Verb {
        domain,
        name: name.into(),
        body,
    })
    .unwrap();
}
#[test]
#[ignore = "requires built Swift helper and live macOS desktop"]
fn native_page_state_navigation_scroll_and_rejection_log() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first.html");
    let second = dir.path().join("second.html");
    std::fs::write(&first, "<!doctype html><title>First page</title><style>html,body{margin:0}body{width:1400px;height:2200px}</style><h1>First</h1><script>setTimeout(()=>{document.title='Updated title';document.body.style.height='2400px'},1500)</script>").unwrap();
    std::fs::write(
        &second,
        "<!doctype html><title>Second page</title><h1>Second</h1>",
    )
    .unwrap();
    let log = dir.path().join("console.log");
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_luchs"))
            .args(["--size=640x480"])
            .arg(&first)
            .env("LUCHS_CONSOLE_LOG", &log)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut endpoint = String::new();
    BufReader::new(process.0.stdout.take().unwrap())
        .read_line(&mut endpoint)
        .unwrap();
    let connected = connect_v2(
        Stream::connect(endpoint.trim()).unwrap(),
        InputRequest::None,
        ChannelRequest::Required,
    )
    .unwrap();
    let host = connected.affordances.unwrap();
    // SAFETY: the production source owns the arena; this test keeps grants private.
    let mut setup = unsafe { CpuSetupClient::from_stream(connected.media) };
    let consumer = setup.attach(1).unwrap();
    wait(
        &host,
        |s| matches!(s, Snapshot::Window(w) if w.ready && w.title.as_deref() == Some("First page")),
    );
    wait(
        &host,
        |s| matches!(s, Snapshot::Navigation(n) if n.title.as_deref() == Some("Updated title") && !n.loading),
    );
    let Snapshot::Scroll(extents) = wait(
        &host,
        |s| matches!(s, Snapshot::Scroll(s) if s.y.content_length >= 2400.),
    ) else {
        unreachable!()
    };
    verb(
        &host,
        Domain::Scroll,
        "scroll_by_step",
        json!({"axis":"y","step":"small","direction":"increment"}),
    );
    wait(
        &host,
        |s| matches!(s, Snapshot::Scroll(s) if s.y.position == 40.),
    );
    verb(
        &host,
        Domain::Scroll,
        "scroll_by_step",
        json!({"axis":"y","step":"large","direction":"increment"}),
    );
    wait(
        &host,
        |s| matches!(s, Snapshot::Scroll(s) if (s.y.position - (40. + extents.y.viewport_length * 0.9)).abs() <= 1.),
    );
    verb(
        &host,
        Domain::Scroll,
        "set_position",
        json!({"axis":"y","position":99999.}),
    );
    wait(
        &host,
        |s| matches!(s, Snapshot::Scroll(s) if s.y.position == extents.y.content_length - extents.y.viewport_length),
    );
    verb(
        &host,
        Domain::Scroll,
        "set_position",
        json!({"axis":"x","position":200.}),
    );
    wait(
        &host,
        |s| matches!(s, Snapshot::Scroll(s) if s.x.position == 200.),
    );
    verb(
        &host,
        Domain::Navigation,
        "load",
        json!({"url":Url::from_file_path(&second).unwrap().as_str()}),
    );
    wait(
        &host,
        |s| matches!(s, Snapshot::Navigation(n) if n.title.as_deref() == Some("Second page") && n.can_go_back && !n.loading),
    );
    verb(&host, Domain::Navigation, "back", json!({}));
    wait(
        &host,
        |s| matches!(s, Snapshot::Navigation(n) if n.url.as_deref().is_some_and(|url| url.ends_with("first.html")) && n.can_go_forward && !n.loading),
    );
    wait(
        &host,
        |s| matches!(s, Snapshot::Scroll(s) if s.y.content_length >= 2400. && s.y.position > 0.),
    );
    verb(&host, Domain::Navigation, "forward", json!({}));
    wait(
        &host,
        |s| matches!(s, Snapshot::Navigation(n) if n.title.as_deref() == Some("Second page") && !n.loading),
    );
    let finishes = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .matches("navigation finish")
            .count()
    };
    let before = finishes();
    verb(&host, Domain::Navigation, "reload", json!({}));
    let deadline = Instant::now() + Duration::from_secs(10);
    while finishes() <= before {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    verb(
        &host,
        Domain::Navigation,
        "load",
        json!({"url":"javascript:alert(1)"}),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !std::fs::read_to_string(&log)
        .unwrap_or_default()
        .contains("rejected navigation.load: javascript:")
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!host.finished());
    drop((host, consumer, setup));
    // SAFETY: this signal targets the test's own child.
    unsafe {
        libc::kill(process.0.id() as i32, libc::SIGTERM);
    }
    assert!(process.0.wait().unwrap().success());
}
