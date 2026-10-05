mod common;

use jackstay::{
    acquisition::socket::CpuSetupClient,
    affordances::{Domain, Event, Host, Snapshot, Verb},
    bootstrap::{ChannelRequest, InputRequest, connect_v2},
    local::Stream,
};
use luchs::{
    affordances::{LoadPolicy, PageState},
    helper::Helper,
    source::Source,
};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
    os::unix::fs::{PermissionsExt, symlink},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use url::Url;

fn wait(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(6);
    while !check() {
        assert!(Instant::now() < deadline, "affordance timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn snapshot(host: &Host, mut check: impl FnMut(&Snapshot) -> bool) -> Snapshot {
    let mut found = None;
    wait(|| {
        while let Some(event) = host.poll() {
            if let Event::Snapshot(value) = event {
                if check(&value) {
                    found = Some(value);
                    return true;
                }
            }
        }
        assert!(!host.finished(), "affordances unexpectedly closed");
        false
    });
    found.unwrap()
}
fn send(host: &Host, domain: Domain, name: &str, body: Value) {
    host.send(Verb {
        domain,
        name: name.into(),
        body,
    })
    .unwrap();
}
fn state(page: &PageState, domain: &str, body: Value) {
    page.helper_state(
        json!({"domain": domain, "body": body})
            .as_object()
            .unwrap()
            .clone(),
    );
}
fn window(title: &str, ready: bool) -> Value {
    json!({"title":title, "requested_size":null, "ready":ready})
}
fn navigation(title: &str) -> Value {
    json!({"title":title,"url":"https://example.test/page","can_go_back":true,"can_go_forward":false,"loading":false,"capabilities":{}})
}
fn scroll(position: f64, scrollable: bool) -> Value {
    let axis = json!({"scrollable":scrollable,"content_length":1000.,"viewport_length":200.,"position":position});
    json!({"x":axis, "y":axis, "capabilities":{}})
}

#[test]
fn load_policy_accept_reject_table() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("site");
    std::fs::create_dir(&root).unwrap();
    let start = root.join("index.html");
    let sibling = root.join("space name.html");
    let outside = dir.path().join("outside.html");
    for file in [&start, &sibling, &outside] {
        std::fs::write(file, "page").unwrap();
    }
    symlink(&outside, root.join("escape.html")).unwrap();
    symlink(dir.path(), root.join("escape-dir")).unwrap();
    let prefix = dir.path().join("site-other");
    std::fs::create_dir(&prefix).unwrap();
    std::fs::write(prefix.join("page.html"), "page").unwrap();
    let policy = LoadPolicy::new(Some(&start)).unwrap();
    fn file(path: impl AsRef<std::path::Path>) -> String {
        Url::from_file_path(path).unwrap().to_string()
    }
    let cases = vec![
        ("https://example.com/page?q=1#part".into(), true),
        ("http://localhost:8080".into(), true),
        ("HTTPS://example.com".into(), true),
        (file(&sibling), true),
        (format!("{}#part", file(&sibling)), true),
        (file(&outside), false),
        (file(root.join("escape.html")), false),
        (file(root.join("escape-dir/outside.html")), false),
        (file(prefix.join("page.html")), false),
        (file(root.join("missing.html")), false),
        (
            format!(
                "{}/../outside.html",
                Url::from_directory_path(&root)
                    .unwrap()
                    .as_str()
                    .trim_end_matches('/')
            ),
            false,
        ),
        (
            format!(
                "{}%2e%2e/outside.html",
                Url::from_directory_path(&root).unwrap()
            ),
            false,
        ),
        ("file://remote/tmp/page.html".into(), false),
        ("javascript:alert(1)".into(), false),
        ("data:text/html,hello".into(), false),
        ("ftp://example.com".into(), false),
        ("about:blank".into(), false),
        ("/tmp/page.html".into(), false),
        ("//example.com/page".into(), false),
        ("https://".into(), false),
        ("not a URL".into(), false),
    ];
    for (value, expected) in cases {
        assert_eq!(policy.allowed_url(&value).is_some(), expected, "{value}");
    }
    let remote = LoadPolicy::new(None).unwrap();
    assert!(remote.allowed_url(&file(&start)).is_none());
    assert!(remote.allowed_url("https://example.com").is_some());
}

#[test]
fn startup_path_without_parent_returns_an_error() {
    let error = match LoadPolicy::new(Some(std::path::Path::new("/"))) {
        Err(error) => error,
        Ok(_) => panic!("filesystem root has no parent"),
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn replacement_arriving_before_poll_still_withdraws_before_fresh_snapshots() {
    let page = PageState::default();
    let publish_domains = |title: &str| {
        state(&page, "window", window(title, true));
        state(&page, "navigation", navigation(title));
        state(&page, "cursor", json!({"shape":"text"}));
        state(&page, "scroll", scroll(20., true));
    };
    page.frame_published();
    publish_domains("old");
    assert_eq!(page.snapshots().len(), 4);
    page.verb(
        Verb {
            domain: Domain::Navigation,
            name: "reload".into(),
            body: json!({}),
        },
        &LoadPolicy::new(None).unwrap(),
    );
    page.helper_stopped();
    publish_domains("new");
    assert!(page.command().is_none());
    assert_eq!(
        page.snapshots(),
        [
            Domain::Window,
            Domain::Navigation,
            Domain::Cursor,
            Domain::Scroll
        ]
        .map(Snapshot::Withdraw)
    );
    let fresh = page.snapshots();
    assert_eq!(fresh.len(), 4);
    assert!(fresh.iter().any(
        |s| matches!(s, Snapshot::Window(w) if w.title.as_deref() == Some("new") && !w.ready)
    ));
    assert!(
        fresh
            .iter()
            .any(|s| matches!(s, Snapshot::Navigation(n) if n.title.as_deref() == Some("new")))
    );
    assert!(fresh.iter().all(|s| !matches!(s, Snapshot::Withdraw(_))));
}

#[test]
fn normalization_readiness_unknown_verbs_and_bounded_queue() {
    let page = PageState::default();
    state(&page, "window", window("first", true));
    assert!(matches!(&page.snapshots()[0], Snapshot::Window(w) if !w.ready));
    page.frame_published();
    assert!(matches!(&page.snapshots()[0], Snapshot::Window(w) if w.ready));
    state(&page, "cursor", json!({"shape":"made-up"}));
    assert_eq!(page.snapshots(), [Snapshot::Cursor("default".into())]);
    state(&page, "scroll", scroll(9999., true));
    assert!(
        matches!(&page.snapshots()[0], Snapshot::Scroll(s) if s.x.position == 800. && s.y.position == 800.)
    );
    state(&page, "scroll", scroll(-20., false));
    assert!(
        matches!(&page.snapshots()[0], Snapshot::Scroll(s) if s.x.position == 0. && !s.x.scrollable)
    );
    page.helper_state(json!({"capture_changed":true}).as_object().unwrap().clone());
    let policy = LoadPolicy::new(None).unwrap();
    page.verb(
        Verb {
            domain: Domain::Navigation,
            name: "unknown".into(),
            body: json!({}),
        },
        &policy,
    );
    page.verb(
        Verb {
            domain: Domain::Media,
            name: "play".into(),
            body: json!({}),
        },
        &policy,
    );
    assert!(page.command().is_none());
    page.verb(
        Verb {
            domain: Domain::Navigation,
            name: "load".into(),
            body: json!({"url": format!("https://example.test/{}", "é".repeat(30000))}),
        },
        &policy,
    );
    assert_eq!(page.command().unwrap()["type"], "navigation.rejected");
    for _ in 0..70 {
        page.verb(
            Verb {
                domain: Domain::Navigation,
                name: "reload".into(),
                body: json!({}),
            },
            &policy,
        );
    }
    assert_eq!(std::iter::from_fn(|| page.command()).count(), 64);
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn commands(path: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

#[test]
fn fake_helper_and_required_host_publish_changes_and_execute_verbs() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("helper");
    let log = dir.path().join("commands");
    let html = dir.path().join("page.html");
    std::fs::write(&html, "fixture").unwrap();
    let initial = [
        json!({"domain":"window","body":window("initial", true)}),
        json!({"domain":"navigation","body":navigation("initial")}),
        json!({"domain":"cursor","body":{"shape":"pointer"}}),
        json!({"domain":"scroll","body":scroll(0., true)}),
    ];
    std::fs::write(
        &script,
        format!(
            "#!/usr/bin/env python3\n{}\ninitial = {}\n{}",
            common::PYTHON_PROTOCOL,
            serde_json::to_string(&serde_json::to_string(&initial).unwrap()).unwrap(),
            r#"
for value in json.loads(initial): control(3, value)
frame()
while True:
    try: cmd = raw_command()
    except EOFError: break
    with open(os.environ['COMMAND_LOG'], 'a') as log: log.write(json.dumps(cmd) + '\n')
    if cmd['type'] == 'navigation.reload':
        for value in json.loads(initial):
            if value['domain'] in ['window', 'navigation']: value['body']['title'] = 'changed'
            if value['domain'] == 'cursor': value['body']['shape'] = 'text'
            if value['domain'] == 'scroll': value['body']['y']['position'] = 400
            control(3, value)
    ack(cmd)
"#
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_luchs"))
            .args(["--size=1x1", "--helper"])
            .arg(&script)
            .arg(&html)
            .env("COMMAND_LOG", &log)
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
        InputRequest::None,
        ChannelRequest::Required,
    )
    .unwrap();
    let host = connected.affordances.unwrap();
    // SAFETY: this test owns a conforming source and the received arena mappings.
    let mut setup = unsafe { CpuSetupClient::from_stream(connected.media) };
    let consumer = setup.attach(1).unwrap();
    let mut domains = std::collections::BTreeSet::new();
    let mut ready = false;
    wait(|| {
        while let Some(Event::Snapshot(value)) = host.poll() {
            domains.insert(value.domain());
            if let Snapshot::Window(w) = value {
                ready |= w.ready;
                assert!(w.requested_size.is_none());
            }
        }
        domains.len() == 4 && ready
    });
    send(&host, Domain::Navigation, "reload", json!({}));
    let mut changed = std::collections::BTreeSet::new();
    wait(|| {
        while let Some(Event::Snapshot(value)) = host.poll() {
            let matches = match &value {
                Snapshot::Window(w) => w.title.as_deref() == Some("changed"),
                Snapshot::Navigation(n) => {
                    n.title.as_deref() == Some("changed") && n.capabilities.len() == 5
                }
                Snapshot::Cursor(s) => s == "text",
                Snapshot::Scroll(s) => s.y.position == 400. && s.capabilities.len() == 2,
                _ => false,
            };
            if matches {
                changed.insert(value.domain());
            }
        }
        changed.len() == 4
    });
    for name in ["back", "forward", "stop"] {
        send(&host, Domain::Navigation, name, json!({}));
    }
    send(
        &host,
        Domain::Navigation,
        "load",
        json!({"url":"https://example.com/new"}),
    );
    send(
        &host,
        Domain::Navigation,
        "load",
        json!({"url":"javascript:alert(1)"}),
    );
    send(
        &host,
        Domain::Scroll,
        "set_position",
        json!({"axis":"x", "position":300.}),
    );
    send(
        &host,
        Domain::Scroll,
        "scroll_by_step",
        json!({"axis":"y", "step":"large", "direction":"decrement"}),
    );
    wait(|| {
        commands(&log)
            .iter()
            .filter(|c| c["type"] != "draw" && c["type"] != "presentation")
            .count()
            == 8
    });
    let recorded = commands(&log);
    for kind in [
        "navigation.back",
        "navigation.forward",
        "navigation.stop",
        "navigation.load",
        "navigation.rejected",
        "scroll.set_position",
        "scroll.scroll_by_step",
    ] {
        assert!(
            recorded.iter().any(|c| c["type"] == kind),
            "missing {kind}: {recorded:?}"
        );
    }
    assert!(
        recorded.iter().any(|c| c["type"] == "scroll.set_position"
            && c["axis"] == "x"
            && c["position"] == 300.)
    );
    assert!(recorded.iter().any(|c| c["type"] == "scroll.scroll_by_step"
        && c["step"] == "large"
        && c["direction"] == "decrement"));
    drop((host, consumer, setup));
    // SAFETY: this is our own child process.
    unsafe {
        libc::kill(process.0.id() as i32, libc::SIGTERM);
    }
    assert!(process.0.wait().unwrap().success());
}

#[test]
fn helper_replacement_withdraws_all_domains_before_fresh_state() {
    let (source, path) =
        Source::bind(&format!("luchs-restart-{}", std::process::id()), 1, 1).unwrap();
    let spawn = |title: &str| {
        let mut records = Vec::new();
        for (domain, body) in [
            ("window", window(title, true)),
            ("navigation", navigation(title)),
            ("cursor", json!({"shape":"text"})),
            ("scroll", scroll(20., true)),
        ] {
            records.extend(common::control(3, json!({"domain":domain, "body":body})));
        }
        let page = source.page.clone();
        Helper::spawn_with_state(
            Command::new("sh")
                .arg("-c")
                .arg(format!("{}; sleep 20", common::socket_write(&records))),
            move |value| page.helper_state(value),
        )
        .unwrap()
    };
    let helper = spawn("old");
    let connected = connect_v2(
        Stream::connect(&path).unwrap(),
        InputRequest::None,
        ChannelRequest::Required,
    )
    .unwrap();
    let host = connected.affordances.unwrap();
    snapshot(
        &host,
        |s| matches!(s, Snapshot::Navigation(n) if n.title.as_deref() == Some("old")),
    );
    // Ensure all four domains have reached the toolkit cache.
    std::thread::sleep(Duration::from_millis(50));
    while host.poll().is_some() {}
    drop(helper);
    source.helper_stopped();
    let mut withdrawn = std::collections::BTreeSet::new();
    wait(|| {
        while let Some(Event::Snapshot(Snapshot::Withdraw(domain))) = host.poll() {
            withdrawn.insert(domain);
        }
        withdrawn.len() == 4
    });
    let helper = spawn("new");
    let mut fresh_window = false;
    let mut fresh_navigation = false;
    wait(|| {
        while let Some(Event::Snapshot(snapshot)) = host.poll() {
            match snapshot {
                Snapshot::Window(w) if w.title.as_deref() == Some("new") => {
                    assert!(!w.ready);
                    fresh_window = true;
                }
                Snapshot::Navigation(n) if n.title.as_deref() == Some("new") => {
                    fresh_navigation = true
                }
                _ => {}
            }
        }
        fresh_window && fresh_navigation
    });
    drop((helper, host, connected.media));
    source.stop().unwrap();
}
