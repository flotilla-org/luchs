//! Complete page-domain snapshots and bounded host-to-helper commands.
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use jackstay::affordances::{Axis, CURSORS, Domain, Navigation, Scroll, Snapshot, Verb, Window};
use serde_json::{Map, Value, json};
use url::Url;

// Inserting a u64 command ID adds one comma, five bytes for `"id":`, and
// at most 20 decimal digits. MAX_CONTROL_BYTES bounds JSON, not its u32 prefix.
const COMMAND_ID_JSON_HEADROOM: usize = 26;

/// HTTP(S) loads may target any host, including from a local startup page.
/// File loads grant no authority outside the original local page directory.
pub struct LoadPolicy {
    directory: Option<PathBuf>,
}
impl LoadPolicy {
    pub fn new(page: Option<&Path>) -> std::io::Result<Self> {
        Ok(Self {
            directory: page
                .map(|page| {
                    page.canonicalize().and_then(|page| {
                        page.parent().map(Path::to_owned).ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidInput,
                                "startup page has no parent directory",
                            )
                        })
                    })
                })
                .transpose()?,
        })
    }

    pub fn allowed_url(&self, value: &str) -> Option<String> {
        let url = Url::parse(value).ok()?;
        match url.scheme() {
            "http" | "https" if url.host_str().is_some() => Some(url.into()),
            "file" => {
                let directory = self.directory.as_ref()?;
                let path = url.to_file_path().ok()?.canonicalize().ok()?;
                if !path.starts_with(directory) {
                    return None;
                }
                let mut canonical = Url::from_file_path(path).ok()?;
                canonical.set_query(url.query());
                canonical.set_fragment(url.fragment());
                Some(canonical.into())
            }
            _ => None,
        }
    }
}

#[derive(Default)]
struct State {
    snapshots: BTreeMap<Domain, Snapshot>,
    dirty: BTreeMap<Domain, Snapshot>,
    withdrawals: Vec<Snapshot>,
    commands: VecDeque<Value>,
    navigation_finished: bool,
    frame_published: bool,
}

#[derive(Clone, Default)]
pub struct PageState(Arc<Mutex<State>>);
impl PageState {
    pub fn helper_state(&self, state: Map<String, Value>) {
        let Some(body) = state.get("body") else {
            return;
        };
        let snapshot = match state.get("domain").and_then(Value::as_str) {
            Some("window") => {
                let Ok(mut window) = serde_json::from_value::<Window>(body.clone()) else {
                    return;
                };
                window.requested_size = None;
                let mut state = self.0.lock().unwrap();
                // Readiness latches for this helper lifetime after the first
                // completed navigation and published frame. Later navigation
                // changes navigation.loading; helper_stopped resets readiness.
                state.navigation_finished |= window.ready;
                window.ready = state.navigation_finished && state.frame_published;
                insert(&mut state, Snapshot::Window(window));
                return;
            }
            Some("navigation") => {
                let Ok(mut navigation) = serde_json::from_value::<Navigation>(body.clone()) else {
                    return;
                };
                navigation.capabilities = ["back", "forward", "reload", "stop", "load"]
                    .map(|name| (name.into(), true))
                    .into();
                Snapshot::Navigation(navigation)
            }
            Some("cursor") => {
                let Some(shape) = body["shape"].as_str() else {
                    return;
                };
                Snapshot::Cursor(
                    if CURSORS.contains(&shape) && shape != "auto" {
                        shape
                    } else {
                        "default"
                    }
                    .into(),
                )
            }
            Some("scroll") => {
                let Ok(mut scroll) = serde_json::from_value::<Scroll>(body.clone()) else {
                    return;
                };
                if !normalize_axis(&mut scroll.x) || !normalize_axis(&mut scroll.y) {
                    return;
                }
                scroll.capabilities = ["scroll_by_step", "set_position"]
                    .map(|name| (name.into(), true))
                    .into();
                Snapshot::Scroll(scroll)
            }
            _ => return,
        };
        insert(&mut self.0.lock().unwrap(), snapshot);
    }

    /// Called when the toolkit returns the first published frame storage.
    pub fn frame_published(&self) {
        let mut state = self.0.lock().unwrap();
        state.frame_published = true;
        if state.navigation_finished {
            if let Some(Snapshot::Window(mut window)) =
                state.snapshots.get(&Domain::Window).cloned()
            {
                window.ready = true;
                insert(&mut state, Snapshot::Window(window));
            }
        }
    }

    /// Call before attaching a replacement helper. Old page state and queued
    /// commands must not leak into the replacement engine's lifetime.
    pub fn helper_stopped(&self) {
        let mut state = self.0.lock().unwrap();
        *state = State {
            withdrawals: [
                Domain::Window,
                Domain::Navigation,
                Domain::Cursor,
                Domain::Scroll,
            ]
            .map(Snapshot::Withdraw)
            .into(),
            ..State::default()
        };
    }

    pub fn snapshots(&self) -> Vec<Snapshot> {
        let mut state = self.0.lock().unwrap();
        // Give withdrawals their own pump iteration even when replacement
        // state arrives before the old domains have been withdrawn.
        if !state.withdrawals.is_empty() {
            return std::mem::take(&mut state.withdrawals);
        }
        std::mem::take(&mut state.dirty).into_values().collect()
    }

    pub fn verb(&self, verb: Verb, policy: &LoadPolicy) {
        let mut command = match (verb.domain, verb.name.as_str()) {
            (Domain::Navigation, "back" | "forward" | "reload" | "stop") => {
                json!({"type": format!("navigation.{}", verb.name)})
            }
            (Domain::Navigation, "load") => {
                let Some(value) = verb.body["url"].as_str() else {
                    return;
                };
                match policy.allowed_url(value) {
                    Some(url) => json!({"type": "navigation.load", "url": url}),
                    // The helper owns the configured console log. Keep rejected
                    // loads on its ordered command path to reach that file.
                    None => json!({"type": "navigation.rejected", "url": value}),
                }
            }
            (Domain::Scroll, "set_position" | "scroll_by_step") => {
                let Some(axis @ ("x" | "y")) = verb.body["axis"].as_str() else {
                    return;
                };
                if verb.name == "set_position" {
                    let Some(position) = verb.body["position"].as_f64().filter(|p| p.is_finite())
                    else {
                        return;
                    };
                    json!({"type": "scroll.set_position", "axis": axis, "position": position})
                } else {
                    let Some(step @ ("small" | "large")) = verb.body["step"].as_str() else {
                        return;
                    };
                    let Some(direction @ ("increment" | "decrement")) =
                        verb.body["direction"].as_str()
                    else {
                        return;
                    };
                    json!({"type": "scroll.scroll_by_step", "axis": axis, "step": step, "direction": direction})
                }
            }
            _ => return,
        };
        // URL normalization can expand UTF-8 into percent-encoded bytes. A
        // valid host record must not turn into a fatal oversized helper write.
        if serde_json::to_vec(&command).unwrap().len() + COMMAND_ID_JSON_HEADROOM
            > crate::protocol::MAX_CONTROL_BYTES
        {
            command =
                json!({"type":"navigation.rejected", "url":"URL exceeds helper command limit"});
        }
        let mut state = self.0.lock().unwrap();
        if state.commands.len() < crate::helper::MAX_PENDING_COMMANDS {
            state.commands.push_back(command);
        } else {
            eprintln!("luchs: affordance command queue full; ignoring verb");
        }
    }

    pub fn command(&self) -> Option<Value> {
        self.0.lock().unwrap().commands.pop_front()
    }
}

fn insert(state: &mut State, snapshot: Snapshot) {
    if state.snapshots.get(&snapshot.domain()) != Some(&snapshot) {
        state.dirty.insert(snapshot.domain(), snapshot.clone());
        state.snapshots.insert(snapshot.domain(), snapshot);
    }
}

fn normalize_axis(axis: &mut Axis) -> bool {
    if ![axis.content_length, axis.viewport_length, axis.position]
        .into_iter()
        .all(f64::is_finite)
        || axis.content_length < 0.
        || axis.viewport_length < 0.
    {
        return false;
    }
    axis.scrollable &= axis.content_length > axis.viewport_length;
    axis.position = if axis.scrollable {
        axis.position
            .clamp(0., (axis.content_length - axis.viewport_length).max(0.))
    } else {
        0.
    };
    true
}
