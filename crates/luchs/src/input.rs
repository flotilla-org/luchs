//! Jackstay work to acknowledged native helper commands.
use crate::{
    helper::{COMMAND_TIMEOUT, CommandSender},
    keymap,
};
use jackstay::input::{Action, Event, Key, Mode, Operation, Outcome, Scope, ScrollUnit, Work};
use serde_json::{Value, json};
use std::time::Duration;

pub struct Executor {
    sender: Option<CommandSender>,
    timeout: Duration,
    viewport_height: f64,
    scroll: ScrollRemainder,
}
impl Executor {
    pub fn new(viewport_height: f64) -> Self {
        Self {
            sender: None,
            timeout: COMMAND_TIMEOUT,
            viewport_height,
            scroll: ScrollRemainder::default(),
        }
    }
    pub fn attach(&mut self, sender: CommandSender) {
        self.sender = Some(sender);
    }
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    pub fn execute(&mut self, work: Work) -> Outcome {
        let mut next_scroll = self.scroll;
        let command = match work.operation {
            Operation::Cleanup { scope, .. } => {
                // With no renderer attached there can be no native holds.
                if self.sender.is_none() {
                    return Outcome::Executed;
                }
                next_scroll = ScrollRemainder::default();
                json!({"type":"cleanup", "scope": if scope == Scope::All {"all"} else {"pointer"}})
            }
            Operation::Event(event) => match self.command(work.mode, event, &mut next_scroll) {
                Some(command) => command,
                None => return Outcome::Unsupported,
            },
        };
        let Some(sender) = &self.sender else {
            return Outcome::Unsupported;
        };
        let outcome = sender
            .send_json_command(command, self.timeout)
            .map_or(Outcome::Uncertain, |pending| {
                pending.wait().execution_outcome()
            });
        if outcome == Outcome::Executed {
            self.scroll = next_scroll;
        }
        outcome
    }
    fn command(&self, mode: Mode, event: Event, scroll: &mut ScrollRemainder) -> Option<Value> {
        Some(match event {
            Event::Motion(p) => json!({"type":"mouse_move","x":p.x,"y":p.y}),
            Event::Button {
                button,
                action,
                position: p,
            } => {
                if !(1..=5).contains(&button) || action == Action::Repeat {
                    return None;
                }
                json!({"type":if action == Action::Up {"mouse_up"} else {"mouse_down"},"button":button,"x":p.x,"y":p.y})
            }
            Event::Scroll {
                x,
                y,
                unit,
                position: p,
            } => {
                let factor = match unit {
                    ScrollUnit::Pixel => 1.0,
                    ScrollUnit::Line => 40.0,
                    ScrollUnit::Page => self.viewport_height,
                };
                let (dx, dy) = (x * factor, y * factor);
                // Quartz fixed-point fields have signed 16.16 range.
                if !dx.is_finite() || !dy.is_finite() || dx.abs() > 32767.0 || dy.abs() > 32767.0 {
                    return None;
                }
                let (px, py) = scroll.points(dx, dy);
                json!({"type":"scroll","x":p.x,"y":p.y,"dx":dx,"dy":dy,"point_dx":px,"point_dy":py})
            }
            Event::Text(text) => {
                if mode == Mode::Physical {
                    return None;
                }
                json!({"type":"text","text":text,"cooperative":mode == Mode::Cooperative})
            }
            Event::Key {
                press,
                action,
                key,
                modifiers,
            } => {
                if mode == Mode::SourceText {
                    return None;
                }
                let (vk, logical) = match key {
                    Key::Physical(code) => (keymap::physical(&code)?, None),
                    Key::Logical(key) => {
                        if mode == Mode::Physical {
                            return None;
                        }
                        let (vk, text) = keymap::logical(&key)?;
                        (vk, Some(text))
                    }
                };
                json!({"type":if action == Action::Up {"key_up"} else {"key_down"},
                    "press":press,"key_code":vk,"logical":logical,"modifiers":modifiers,
                    "repeat":action == Action::Repeat,"cooperative":mode == Mode::Cooperative})
            }
        })
    }
}

/// Integer point fields retain fractions across events; fixed-point fields also
/// carry each event's fractional displacement for precise native consumers.
#[derive(Default, Clone, Copy)]
pub struct ScrollRemainder {
    x: f64,
    y: f64,
}
impl ScrollRemainder {
    pub fn points(&mut self, x: f64, y: f64) -> (i64, i64) {
        self.x += x;
        self.y += y;
        let points = (self.x.trunc() as i64, self.y.trunc() as i64);
        self.x -= points.0 as f64;
        self.y -= points.1 as f64;
        points
    }
}
