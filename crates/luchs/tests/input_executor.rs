mod common;
use jackstay::input::*;
use luchs::{
    helper::Helper,
    input::{Executor, ScrollRemainder},
};
use std::{process::Command, time::Duration};

fn fake(script: &str) -> Helper {
    Helper::spawn(
        Command::new("python3").args(["-c", &format!("{}{script}", common::PYTHON_PROTOCOL)]),
    )
    .unwrap()
}
fn work(operation: Operation) -> Work {
    Work {
        mode: Mode::Cooperative,
        id: 1,
        controller: 1,
        epoch: 1,
        sequence: 1,
        operation,
    }
}
fn key(action: Action) -> Event {
    Event::Key {
        press: 42,
        action,
        key: Key::Logical("a".into()),
        modifiers: 0,
    }
}
fn position() -> Position {
    Position {
        revision: 1,
        x: 12.125,
        y: 13.875,
    }
}
fn executor(helper: &Helper) -> Executor {
    let mut executor = Executor::new(600.0).with_timeout(Duration::from_secs(3));
    executor.attach(helper.command_sender());
    executor
}

#[test]
fn acknowledged_input_maps_executed_unsupported_failed_and_timeout() {
    let mut helper = fake(
        r#"
for outcome in ['executed', 'unsupported', 'failed']:
    cmd=command()
    assert cmd['type']=='text' and cmd['text']=='é🙂\u0000'
    ack(cmd,outcome)
cmd=command()
time.sleep(0.5)
ack(cmd)
"#,
    );
    let mut executor = executor(&helper).with_timeout(Duration::from_millis(300));
    for outcome in [
        Outcome::Executed,
        Outcome::Unsupported,
        Outcome::Uncertain,
        Outcome::Uncertain,
    ] {
        assert_eq!(
            executor.execute(work(Operation::Event(Event::Text("é🙂\0".into())))),
            outcome
        );
    }
    helper.finish().unwrap();
    assert_eq!(helper.ignored_acks(), 1);
}

#[test]
fn command_vocabulary_preserves_positions_modes_and_scroll_fractions() {
    let mut helper = fake(
        r#"
cmd=command(); assert cmd['type']=='mouse_down' and cmd['button']==2
assert cmd['x']==12.125 and cmd['y']==13.875; ack(cmd)
cmd=command(); assert cmd['key_code']==0 and cmd['logical']=='a' and cmd['press']==42; ack(cmd)
cmd=command(); assert cmd['key_code']==8 and cmd['logical'] is None and not cmd['cooperative']; ack(cmd)
for unit,dy,point in [('pixel',0.25,0),('pixel',0.75,1),('line',20,20),('page',300,300)]:
    cmd=command(); assert cmd['type']=='scroll' and cmd['dy']==dy and cmd['point_dy']==point; ack(cmd)
cmd=command(); assert cmd['type']=='text' and not cmd['cooperative']; ack(cmd)
"#,
    );
    let mut e = executor(&helper);
    assert_eq!(
        e.execute(work(Operation::Event(Event::Button {
            button: 2,
            action: Action::Down,
            position: position()
        }))),
        Outcome::Executed
    );
    assert_eq!(
        e.execute(work(Operation::Event(key(Action::Down)))),
        Outcome::Executed
    );
    let mut physical = work(Operation::Event(Event::Key {
        press: 43,
        action: Action::Down,
        key: Key::Physical("KeyC".into()),
        modifiers: 8,
    }));
    physical.mode = Mode::Physical;
    assert_eq!(e.execute(physical), Outcome::Executed);
    for (unit, y) in [
        (ScrollUnit::Pixel, 0.25),
        (ScrollUnit::Pixel, 0.75),
        (ScrollUnit::Line, 0.5),
        (ScrollUnit::Page, 0.5),
    ] {
        assert_eq!(
            e.execute(work(Operation::Event(Event::Scroll {
                x: 0.0,
                y,
                unit,
                position: position()
            }))),
            Outcome::Executed
        );
    }
    let mut text = work(Operation::Event(Event::Text("commit".into())));
    text.mode = Mode::SourceText;
    assert_eq!(e.execute(text), Outcome::Executed);
    assert_eq!(
        e.execute(work(Operation::Event(Event::Key {
            press: 77,
            action: Action::Down,
            key: Key::Physical("MadeUp".into()),
            modifiers: 0
        }))),
        Outcome::Unsupported
    );
    helper.finish().unwrap();
}

fn pump(target: &Target, e: &mut Executor) {
    let w = target.next().unwrap();
    let id = w.id;
    let outcome = e.execute(w);
    assert_eq!(outcome, Outcome::Executed);
    target.complete(id, outcome).unwrap();
}
#[test]
fn replacement_waits_for_helper_cleanup_ack_and_geometry_cleans_only_pointer() {
    let mut helper = fake(
        r#"
cmd=command(); assert cmd['type']=='mouse_down'; ack(cmd)
cmd=command(); assert cmd['type']=='key_down'; ack(cmd)
cmd=command(); assert cmd['type']=='cleanup' and cmd['scope']=='pointer'; ack(cmd)
cmd=command(); assert cmd['type']=='key_up' and cmd['key_code']==0 and cmd['press']==42; ack(cmd)
cmd=command(); assert cmd['type']=='cleanup' and cmd['scope']=='all'
control(3,{'cleanup_started':True})
time.sleep(0.15)
ack(cmd)
"#,
    );
    let target = Target::new(Config {
        modes: 7,
        ..Config::default()
    })
    .unwrap();
    let controller = target.admit(Mode::Cooperative).unwrap();
    let mut e = executor(&helper);
    controller
        .submit(
            1,
            1,
            Event::Button {
                button: 1,
                action: Action::Down,
                position: position(),
            },
        )
        .unwrap();
    pump(&target, &mut e);
    controller.submit(1, 2, key(Action::Down)).unwrap();
    pump(&target, &mut e);
    target
        .set_geometry(Geometry {
            revision: 2,
            width: 800.0,
            height: 600.0,
        })
        .unwrap();
    pump(&target, &mut e);
    // The library binds release through press identity despite changed key metadata.
    controller
        .submit(
            controller.epoch().unwrap(),
            3,
            Event::Key {
                press: 42,
                action: Action::Up,
                key: Key::Logical("z".into()),
                modifiers: 0,
            },
        )
        .unwrap();
    pump(&target, &mut e);
    controller.close();
    let w = target.next().unwrap();
    let id = w.id;
    let handle = std::thread::spawn(move || e.execute(w));
    assert_eq!(target.admit(Mode::Cooperative).err(), Some(Error::Busy));
    std::thread::sleep(Duration::from_millis(50));
    assert!(!handle.is_finished());
    assert_eq!(target.admit(Mode::Cooperative).err(), Some(Error::Busy));
    assert_eq!(handle.join().unwrap(), Outcome::Executed);
    // Ack alone is not the library barrier: completion must also be recorded.
    assert_eq!(target.admit(Mode::Cooperative).err(), Some(Error::Busy));
    target.complete(id, Outcome::Executed).unwrap();
    assert!(target.admit(Mode::Cooperative).is_ok());
    helper.finish().unwrap();
}

#[test]
fn cleanup_failure_quarantines_replacement() {
    let helper = fake("cmd=command(); assert cmd['type']=='cleanup'; ack(cmd,'failed')");
    let target = Target::new(Config::default()).unwrap();
    let c = target.admit(Mode::Cooperative).unwrap();
    c.close();
    let w = target.next().unwrap();
    let id = w.id;
    assert_eq!(executor(&helper).execute(w), Outcome::Uncertain);
    assert_eq!(
        target.complete(id, Outcome::Uncertain),
        Err(Error::CleanupFailed)
    );
    assert!(target.failed());
    assert!(target.admit(Mode::Cooperative).is_err());
}
#[test]
fn scroll_accumulates_each_axis_and_sign_without_rounding_each_event() {
    let mut remainder = ScrollRemainder::default();
    for _ in 0..3 {
        assert_eq!(remainder.points(0.25, -0.25), (0, 0));
    }
    assert_eq!(remainder.points(0.25, -0.25), (1, -1));
    assert_eq!(remainder.points(-0.75, 0.5), (0, 0));
    assert_eq!(remainder.points(-0.25, 0.5), (-1, 1));
}

fn scroll(x: f64, y: f64, unit: ScrollUnit) -> Work {
    work(Operation::Event(Event::Scroll {
        x,
        y,
        unit,
        position: position(),
    }))
}

#[test]
fn scroll_carry_commits_only_after_executed_ack() {
    let mut helper = fake(
        r#"
cmd=command(); assert cmd['point_dx']==0 and cmd['point_dy']==0; ack(cmd)
for outcome in ['unsupported', 'failed', 'timeout']:
    cmd=command(); assert cmd['point_dx']==1 and cmd['point_dy']==-1
    if outcome=='timeout': time.sleep(0.4); ack(cmd)
    else: ack(cmd,outcome)
cmd=command(); assert cmd['point_dx']==0 and cmd['point_dy']==0; ack(cmd)
cmd=command(); assert cmd['point_dx']==1 and cmd['point_dy']==-1; ack(cmd)
"#,
    );
    let mut e = executor(&helper).with_timeout(Duration::from_millis(300));
    assert_eq!(
        e.execute(scroll(0.25, -0.25, ScrollUnit::Pixel)),
        Outcome::Executed
    );
    for outcome in [Outcome::Unsupported, Outcome::Uncertain, Outcome::Uncertain] {
        assert_eq!(e.execute(scroll(0.75, -0.75, ScrollUnit::Pixel)), outcome);
    }
    assert_eq!(
        e.execute(scroll(0.5, -0.5, ScrollUnit::Pixel)),
        Outcome::Executed
    );
    assert_eq!(
        e.execute(scroll(0.25, -0.25, ScrollUnit::Pixel)),
        Outcome::Executed
    );
    helper.finish().unwrap();
}

#[test]
fn failed_scroll_send_preserves_carry_for_the_next_executed_command() {
    let mut helper = fake(
        r#"
cmd=command(); assert cmd['point_dx']==0 and cmd['point_dy']==0; ack(cmd)
cmd=command(); assert cmd['point_dx']==1 and cmd['point_dy']==-1; ack(cmd)
"#,
    );
    let mut e = executor(&helper);
    assert_eq!(
        e.execute(scroll(0.25, -0.25, ScrollUnit::Pixel)),
        Outcome::Executed
    );
    let mut dead = fake("sys.exit(0)");
    dead.finish().unwrap();
    e.attach(dead.command_sender());
    assert_eq!(
        e.execute(scroll(0.75, -0.75, ScrollUnit::Pixel)),
        Outcome::Uncertain
    );
    e.attach(helper.command_sender());
    assert_eq!(
        e.execute(scroll(0.75, -0.75, ScrollUnit::Pixel)),
        Outcome::Executed
    );
    helper.finish().unwrap();
}

#[test]
fn out_of_range_scroll_is_unsupported_without_sending_or_consuming_carry() {
    let mut helper = fake(
        r#"
cmd=command(); assert cmd['dx']==0.25 and cmd['point_dx']==0; ack(cmd)
cmd=command(); assert cmd['dx']==0.75 and cmd['point_dx']==1; ack(cmd)
cmd=command(); assert cmd['dx']==32767 and cmd['dy']==-32767; ack(cmd)
"#,
    );
    let mut e = executor(&helper);
    assert_eq!(
        e.execute(scroll(0.25, 0.0, ScrollUnit::Pixel)),
        Outcome::Executed
    );
    for (x, y, unit) in [
        (32768.0, 0.0, ScrollUnit::Pixel),
        (0.0, -32768.0, ScrollUnit::Pixel),
        (820.0, 0.0, ScrollUnit::Line),
        (0.0, 55.0, ScrollUnit::Page),
        (f64::INFINITY, 0.0, ScrollUnit::Pixel),
        (0.0, f64::NAN, ScrollUnit::Pixel),
    ] {
        assert_eq!(e.execute(scroll(x, y, unit)), Outcome::Unsupported);
    }
    assert_eq!(
        e.execute(scroll(0.75, 0.0, ScrollUnit::Pixel)),
        Outcome::Executed
    );
    assert_eq!(
        e.execute(scroll(32767.0, -32767.0, ScrollUnit::Pixel)),
        Outcome::Executed
    );
    helper.finish().unwrap();
}

#[test]
fn helper_dying_during_input_leaves_execution_uncertain_and_cleanup_quarantined() {
    let mut helper = fake("cmd=command(); assert cmd['type']=='key_down'; sys.exit(23)");
    let mut e = executor(&helper);
    let target = Target::new(Config::default()).unwrap();
    let controller = target.admit(Mode::Cooperative).unwrap();
    controller.submit(1, 1, key(Action::Down)).unwrap();
    let w = target.next().unwrap();
    let id = w.id;
    assert_eq!(e.execute(w), Outcome::Uncertain);
    target.complete(id, Outcome::Uncertain).unwrap();
    let cleanup = target.next().unwrap();
    let id = cleanup.id;
    assert!(matches!(cleanup.operation, Operation::Cleanup { .. }));
    assert_eq!(e.execute(cleanup), Outcome::Uncertain);
    assert_eq!(
        target.complete(id, Outcome::Uncertain),
        Err(Error::CleanupFailed)
    );
    assert_eq!(
        target.admit(Mode::Cooperative).err(),
        Some(Error::CleanupFailed)
    );
    assert!(helper.finish().unwrap_err().to_string().contains("23"));
}
