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

#[test]
#[ignore = "requires built Swift helper and live macOS desktop"]
fn native_input_and_affordances_share_one_required_host() {
    use jackstay::input::{Action, Event as InputEvent, Mode, Outcome, Position, Status};
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("input-and-state.html");
    let second = dir.path().join("next.html");
    std::fs::write(&first, r#"<!doctype html><title>Combined page</title>
<style>html,body{margin:0}body{height:2000px}input{position:absolute;left:0;top:0;width:300px;height:40px}</style>
<input id="edit">
<script>edit.addEventListener('input',()=>document.title='Typed '+edit.value);window.addEventListener('mousedown',e=>console.log('mousedown '+e.clientX+','+e.clientY+' trusted='+e.isTrusted))</script>"#).unwrap();
    std::fs::write(&second, "<!doctype html><title>Combined next</title>").unwrap();
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_luchs"))
            .arg("--size=640x480")
            .arg(&first)
            .env("LUCHS_CONSOLE_LOG", dir.path().join("console.log"))
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
        InputRequest::Required(Mode::SourceText),
        ChannelRequest::Required,
    )
    .unwrap();
    let host = connected.affordances.unwrap();
    let input = connected.input.unwrap();
    // SAFETY: the production source owns the arena; this test keeps grants private.
    let mut setup = unsafe { CpuSetupClient::from_stream(connected.media) };
    let consumer = setup.attach(1).unwrap();
    wait(&host, |s| matches!(s, Snapshot::Window(w) if w.ready));
    let position = |x, y| Position {
        revision: input.welcome().config.geometry.revision,
        x,
        y,
    };
    let send = |event| {
        let sequence = input.send(event).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(Status::Completed {
                sequence: completed,
                outcome,
            }) = input.poll()
            {
                assert_eq!(completed, sequence);
                assert_eq!(outcome, Outcome::Executed);
                return;
            }
            assert!(
                Instant::now() < deadline,
                "native input acknowledgement timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    send(InputEvent::Button {
        button: 1,
        action: Action::Down,
        position: position(20., 20.),
    });
    send(InputEvent::Button {
        button: 1,
        action: Action::Up,
        position: position(20., 20.),
    });
    wait(
        &host,
        |s| matches!(s, Snapshot::Cursor(shape) if shape == "text"),
    );
    assert!(
        std::fs::read_to_string(dir.path().join("console.log"))
            .unwrap()
            .contains("mousedown 20,20 trusted=true")
    );
    send(InputEvent::Text("é🙂".into()));
    wait(
        &host,
        |s| matches!(s, Snapshot::Window(w) if w.title.as_deref() == Some("Typed é🙂")),
    );
    verb(
        &host,
        Domain::Scroll,
        "set_position",
        json!({"axis":"y","position":40.}),
    );
    wait(
        &host,
        |s| matches!(s, Snapshot::Scroll(s) if s.y.position == 40.),
    );
    verb(
        &host,
        Domain::Navigation,
        "load",
        json!({"url":Url::from_file_path(&second).unwrap().as_str()}),
    );
    wait(
        &host,
        |s| matches!(s, Snapshot::Navigation(n) if n.title.as_deref() == Some("Combined next") && !n.loading),
    );
    input.close();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(Status::Closed { clean, .. }) = input.poll() {
            assert!(clean, "native input cleanup failed");
            break;
        }
        assert!(Instant::now() < deadline, "native input cleanup timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    drop((input, host, consumer, setup));
    // SAFETY: this signal targets the test's own child.
    unsafe {
        libc::kill(process.0.id() as i32, libc::SIGTERM);
    }
    assert!(process.0.wait().unwrap().success());
}

#[test]
#[ignore = "requires built Swift helper and live macOS desktop"]
fn native_host_motion_delivers_trusted_dom_hover() {
    use jackstay::input::{Action, Event as InputEvent, Mode, Outcome, Position, Status};
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("hover.html");
    std::fs::write(&first, r##"<!doctype html><title>Hover probe</title>
<style>html,body{margin:0}input{position:absolute;left:0;top:0;width:300px;height:40px}a{position:absolute;left:0;top:60px;width:200px;height:40px}</style>
<input>
<a href="#next">Hover target</a>
<script>window.addEventListener('mousemove',e=>console.log('mousemove '+e.clientX+','+e.clientY+' trusted='+e.isTrusted+' hover='+document.querySelector('a').matches(':hover')))</script>"##).unwrap();
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_luchs"))
            .arg("--size=640x480")
            .arg(&first)
            .env("LUCHS_CONSOLE_LOG", dir.path().join("console.log"))
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
        InputRequest::Required(Mode::SourceText),
        ChannelRequest::Required,
    )
    .unwrap();
    let host = connected.affordances.unwrap();
    let input = connected.input.unwrap();
    // SAFETY: the production source owns the arena; this test keeps grants private.
    let mut setup = unsafe { CpuSetupClient::from_stream(connected.media) };
    let consumer = setup.attach(1).unwrap();
    wait(&host, |s| matches!(s, Snapshot::Window(w) if w.ready));
    let position = |x, y| Position {
        revision: input.welcome().config.geometry.revision,
        x,
        y,
    };
    let send = |event| {
        let sequence = input.send(event).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(Status::Completed {
                sequence: completed,
                outcome,
            }) = input.poll()
            {
                assert_eq!(completed, sequence);
                assert_eq!(outcome, Outcome::Executed);
                return;
            }
            assert!(
                Instant::now() < deadline,
                "native input acknowledgement timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    send(InputEvent::Motion(position(20., 70.)));
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let log = std::fs::read_to_string(dir.path().join("console.log")).unwrap_or_default();
        if log.contains("mousemove 20,70 trusted=true") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Motion executed but no trusted DOM mousemove 20,70; console:\n{log}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    wait(
        &host,
        |s| matches!(s, Snapshot::Cursor(shape) if shape == "pointer"),
    );
    assert!(
        std::fs::read_to_string(dir.path().join("console.log"))
            .unwrap()
            .contains("mousemove 20,70 trusted=true hover=true")
    );
    send(InputEvent::Motion(position(20., 120.)));
    wait(
        &host,
        |s| matches!(s, Snapshot::Cursor(shape) if shape == "default"),
    );
    send(InputEvent::Button {
        button: 1,
        action: Action::Down,
        position: position(20., 20.),
    });
    send(InputEvent::Button {
        button: 1,
        action: Action::Up,
        position: position(20., 20.),
    });
    wait(
        &host,
        |s| matches!(s, Snapshot::Cursor(shape) if shape == "text"),
    );
    send(InputEvent::Motion(position(20., 70.)));
    wait(
        &host,
        |s| matches!(s, Snapshot::Cursor(shape) if shape == "pointer"),
    );
    let log = std::fs::read_to_string(dir.path().join("console.log")).unwrap();
    assert_eq!(
        log.matches("mousemove 20,70 trusted=true hover=true")
            .count(),
        2
    );
    eprintln!("native hover console:\n{log}");
    input.close();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(Status::Closed { clean, .. }) = input.poll() {
            assert!(clean, "native input cleanup failed");
            break;
        }
        assert!(Instant::now() < deadline, "native input cleanup timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    drop((input, host, consumer, setup));
    // SAFETY: this signal targets the test's own child.
    unsafe {
        libc::kill(process.0.id() as i32, libc::SIGTERM);
    }
    assert!(process.0.wait().unwrap().success());
}
