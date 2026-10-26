//! Finding out which processes komorebi is managing, so each one can get its own OS trace.
//!
//! komorebi's `State` JSON deliberately carries no process id: a `Window` serializes only its
//! `id`, `rect` and `details { title, exe, role, subrole, icon_path }`. So the recorder walks the
//! state generically for window ids and resolves id -> owning pid itself with
//! `CGWindowListCopyWindowInfo(OptionAll, ...)`. `OptionAll` matters: komorebi's own wrapper only
//! lists on-screen windows, which would miss everything on other workspaces and spaces.

use crate::clock;
use objc2_core_foundation::CFArray;
use objc2_core_foundation::CFDictionary;
use objc2_core_foundation::CFNumber;
use objc2_core_foundation::CFString;
use objc2_core_graphics::CGWindowListCopyWindowInfo;
use objc2_core_graphics::CGWindowListOption;
use objc2_core_graphics::kCGNullWindowID;
use objc2_core_graphics::kCGWindowLayer;
use objc2_core_graphics::kCGWindowNumber;
use objc2_core_graphics::kCGWindowOwnerName;
use objc2_core_graphics::kCGWindowOwnerPID;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;
use std::collections::btree_map::Entry;
use std::ptr::NonNull;

/// A window found in a komorebi `State`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateWindow {
    pub id: u32,
    pub exe: Option<String>,
    pub title: Option<String>,
}

/// Collect every window in a `State` without depending on its exact nesting: any object with a
/// numeric `id` and a `details` object counts. Tiled containers, the monocle container, a
/// maximized window and floating windows are all found, and so are layouts that do not exist yet.
#[must_use]
pub fn collect_windows(state: &Value) -> Vec<StateWindow> {
    fn walk(value: &Value, out: &mut BTreeMap<u32, StateWindow>) {
        match value {
            Value::Object(map) => {
                if let (Some(id), Some(Value::Object(details))) =
                    (map.get("id").and_then(Value::as_u64), map.get("details"))
                    && let Ok(id) = u32::try_from(id)
                {
                    let text =
                        |key: &str| details.get(key).and_then(Value::as_str).map(str::to_string);

                    out.entry(id).or_insert_with(|| StateWindow {
                        id,
                        exe: text("exe"),
                        title: text("title"),
                    });
                }

                for child in map.values() {
                    walk(child, out);
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, out);
                }
            }
            _ => {}
        }
    }

    let mut windows = BTreeMap::new();
    walk(state, &mut windows);
    windows.into_values().collect()
}

/// The process that owns a window, as reported by the window server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowOwner {
    pub pid: i32,
    pub name: String,
    pub layer: i64,
}

fn dictionary_value<T>(dictionary: &CFDictionary, key: &CFString) -> Option<NonNull<T>> {
    // SAFETY: `key` is a valid CFString and the dictionary is live
    let value = unsafe { CFDictionary::value(dictionary, NonNull::from(key).as_ptr().cast()) };
    NonNull::new(value.cast_mut()).map(NonNull::cast)
}

/// Every window the window server knows about (all spaces, on-screen or not), keyed by window id.
#[must_use]
pub fn window_owners() -> HashMap<u32, WindowOwner> {
    let mut owners = HashMap::new();
    let Some(list) = CGWindowListCopyWindowInfo(CGWindowListOption::OptionAll, kCGNullWindowID)
    else {
        return owners;
    };

    let list: &CFArray = &list;
    for index in 0..list.count() {
        // SAFETY: every element of the window list is a CFDictionary, and `index` is in bounds
        let Some(dictionary) =
            (unsafe { list.value_at_index(index).cast::<CFDictionary>().as_ref() })
        else {
            continue;
        };

        // SAFETY: values under these keys have these types per the CGWindow documentation
        let (number, pid, name, layer) = unsafe {
            (
                dictionary_value::<CFNumber>(dictionary, kCGWindowNumber)
                    .and_then(|number| number.as_ref().as_i64()),
                dictionary_value::<CFNumber>(dictionary, kCGWindowOwnerPID)
                    .and_then(|pid| pid.as_ref().as_i32()),
                dictionary_value::<CFString>(dictionary, kCGWindowOwnerName)
                    .map(|name| name.as_ref().to_string()),
                dictionary_value::<CFNumber>(dictionary, kCGWindowLayer)
                    .and_then(|layer| layer.as_ref().as_i64()),
            )
        };

        if let (Some(number), Some(pid)) = (number, pid)
            && let Ok(number) = u32::try_from(number)
        {
            owners.insert(
                number,
                WindowOwner {
                    pid,
                    name: name.unwrap_or_default(),
                    layer: layer.unwrap_or_default(),
                },
            );
        }
    }

    owners
}

/// Every process that owns at least one ordinary (layer 0) window, i.e. the apps a user can see.
#[must_use]
pub fn gui_app_pids(owners: &HashMap<u32, WindowOwner>) -> BTreeMap<i32, String> {
    owners
        .values()
        .filter(|owner| owner.layer == 0 && owner.pid > 0)
        .map(|owner| (owner.pid, owner.name.clone()))
        .collect()
}

/// How a window's owning process was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    /// The window id was found in the window server's list.
    WindowId,
    /// The window id was not listed, but exactly one process has a matching owner name.
    OwnerName,
}

/// A process komorebi manages windows of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedApp {
    pub pid: i32,
    pub name: String,
    pub window_ids: BTreeSet<u32>,
    pub resolution: Resolution,
}

/// The outcome of resolving a state's windows to processes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolved {
    pub apps: Vec<ManagedApp>,
    /// Windows in the state that could not be tied to any process.
    pub unresolved: Vec<StateWindow>,
}

/// Resolve state windows to owning processes.
#[must_use]
pub fn resolve(windows: &[StateWindow], owners: &HashMap<u32, WindowOwner>) -> Resolved {
    let mut by_name: HashMap<&str, BTreeSet<i32>> = HashMap::new();
    for owner in owners.values() {
        by_name
            .entry(owner.name.as_str())
            .or_default()
            .insert(owner.pid);
    }

    let mut apps: BTreeMap<i32, ManagedApp> = BTreeMap::new();
    let mut unresolved = Vec::new();

    for window in windows {
        let found = owners
            .get(&window.id)
            .map(|owner| (owner.pid, owner.name.clone(), Resolution::WindowId))
            .or_else(|| {
                let exe = window.exe.as_deref()?;
                let pids = by_name.get(exe)?;
                // only trust a name match when it is unambiguous
                (pids.len() == 1).then(|| {
                    (
                        *pids.iter().next().unwrap_or(&0),
                        exe.to_string(),
                        Resolution::OwnerName,
                    )
                })
            });

        match found {
            Some((pid, name, resolution)) if pid > 0 => {
                apps.entry(pid)
                    .or_insert_with(|| ManagedApp {
                        pid,
                        name,
                        window_ids: BTreeSet::new(),
                        resolution,
                    })
                    .window_ids
                    .insert(window.id);
            }
            _ => unresolved.push(window.clone()),
        }
    }

    Resolved {
        apps: apps.into_values().collect(),
        unresolved,
    }
}

const WINDOW_MANAGER_EVENTS: &[&str] = &[
    "FocusChange",
    "Show",
    "Destroy",
    "Minimize",
    "Restore",
    "Manage",
    "Unmanage",
    "MoveStart",
    "MoveEnd",
    "ResizeStart",
    "ResizeEnd",
];

/// The process id carried by a `WindowManagerEvent` notification (`content[1]`), if the event is
/// one. Used to notice a new process with no polling delay.
#[must_use]
pub fn event_pid(event: &Value) -> Option<i32> {
    let kind = event.get("type")?.as_str()?;
    if !WINDOW_MANAGER_EVENTS.contains(&kind) {
        return None;
    }

    let pid = event.get("content")?.as_array()?.get(1)?.as_i64()?;
    i32::try_from(pid).ok().filter(|pid| *pid > 0)
}

/// A managed process as tracked across a whole recording.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppRecord {
    pub pid: i32,
    pub exe: String,
    pub window_ids: Vec<u32>,
    /// When the recorder first noticed this process; its OS trace starts no earlier than this.
    pub first_seen_mach_ns: u64,
    /// `state`, `event`, `all-gui` or `explicit`.
    pub origin: String,
    pub resolution: Option<Resolution>,
    pub stream_file: Option<String>,
    /// Unified-log events captured for this process.
    #[serde(default)]
    pub events: u64,
    #[serde(default)]
    pub bytes: u64,
}

/// Tracks the distinct managed processes seen over a recording.
#[derive(Debug, Default)]
pub struct AppTracker {
    apps: BTreeMap<i32, AppRecord>,
    unresolved: BTreeMap<u32, StateWindow>,
}

impl AppTracker {
    /// Fold a resolution into the tracker, returning the records for processes seen for the
    /// first time (the caller spawns a stream for each).
    pub fn observe(&mut self, resolved: Resolved, origin: &str) -> Vec<AppRecord> {
        let mut fresh = Vec::new();

        for app in resolved.apps {
            match self.apps.entry(app.pid) {
                Entry::Vacant(slot) => {
                    let record = AppRecord {
                        pid: app.pid,
                        exe: app.name,
                        window_ids: app.window_ids.into_iter().collect(),
                        first_seen_mach_ns: clock::mach_now_ns(),
                        origin: origin.to_string(),
                        resolution: Some(app.resolution),
                        ..AppRecord::default()
                    };
                    fresh.push(record.clone());
                    slot.insert(record);
                }
                Entry::Occupied(mut slot) => {
                    let record = slot.get_mut();
                    for id in app.window_ids {
                        if !record.window_ids.contains(&id) {
                            record.window_ids.push(id);
                        }
                    }
                }
            }
        }

        for window in resolved.unresolved {
            self.unresolved.insert(window.id, window);
        }

        // a window that resolved later is no longer unresolved
        let known: BTreeSet<u32> = self
            .apps
            .values()
            .flat_map(|app| app.window_ids.iter().copied())
            .collect();
        self.unresolved.retain(|id, _| !known.contains(id));

        fresh
    }

    /// Track a process we were told about directly (an event pid, `--trace-pid`, `all-gui`).
    pub fn observe_pid(&mut self, pid: i32, name: String, origin: &str) -> Option<AppRecord> {
        if self.apps.contains_key(&pid) {
            return None;
        }

        let record = AppRecord {
            pid,
            exe: name,
            window_ids: Vec::new(),
            first_seen_mach_ns: clock::mach_now_ns(),
            origin: origin.to_string(),
            resolution: None,
            ..AppRecord::default()
        };
        self.apps.insert(pid, record.clone());
        Some(record)
    }

    pub fn set_stream_file(&mut self, pid: i32, file: &str) {
        if let Some(app) = self.apps.get_mut(&pid) {
            app.stream_file = Some(file.to_string());
        }
    }

    #[must_use]
    pub fn contains(&self, pid: i32) -> bool {
        self.apps.contains_key(&pid)
    }

    #[must_use]
    pub fn records(&self) -> Vec<AppRecord> {
        self.apps.values().cloned().collect()
    }

    #[must_use]
    pub fn unresolved(&self) -> Vec<StateWindow> {
        self.unresolved.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real komorebi `State` (window titles sanitised), captured while reproducing a bug.
    const STATE_REPRO: &str = include_str!("../../tests/fixtures/state.json");

    fn state() -> Value {
        serde_json::from_str(STATE_REPRO).expect("the repro state parses")
    }

    #[test]
    fn walks_every_window_in_a_real_state() {
        let windows = collect_windows(&state());
        let ids: Vec<u32> = windows.iter().map(|window| window.id).collect();

        // spot-check ids from different workspaces
        for id in [59, 61, 8310, 107, 3395, 6293, 8550] {
            assert!(ids.contains(&id), "window {id} missing from {ids:?}");
        }

        let slack = windows
            .iter()
            .find(|window| window.id == 59)
            .expect("slack");
        assert_eq!(slack.exe.as_deref(), Some("Slack"));
        assert!(windows.len() >= 17, "found {}", windows.len());

        // ids are unique
        let unique: BTreeSet<u32> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len());
    }

    #[test]
    fn finds_windows_wherever_they_are_nested() {
        let state: Value = serde_json::from_str(
            r#"{
                "layout": {"deeply": {"nested": [
                    {"id": 1, "details": {"exe": "A", "title": "one"}},
                    {"monocle_container": {"windows": {"elements": [{"id": 2, "details": {"exe": "B"}}]}}},
                    {"maximized_window": {"id": 3, "details": {"exe": "C"}}},
                    {"id": "not-a-window-id", "details": {"exe": "D"}},
                    {"id": 4, "no_details": true}
                ]}},
                "floating_windows": {"elements": [{"id": 5, "details": {"exe": "E"}}]}
            }"#,
        )
        .expect("json");

        let ids: Vec<u32> = collect_windows(&state)
            .iter()
            .map(|window| window.id)
            .collect();
        assert_eq!(ids, vec![1, 2, 3, 5]);
    }

    #[test]
    fn resolves_windows_by_id_across_spaces() {
        let owners = HashMap::from([
            (
                59,
                WindowOwner {
                    pid: 100,
                    name: "Slack".into(),
                    layer: 0,
                },
            ),
            (
                61,
                WindowOwner {
                    pid: 200,
                    name: "Brave".into(),
                    layer: 0,
                },
            ),
            (
                8310,
                WindowOwner {
                    pid: 200,
                    name: "Brave".into(),
                    layer: 0,
                },
            ),
        ]);

        let windows: Vec<StateWindow> = [59, 61, 8310]
            .into_iter()
            .map(|id| StateWindow {
                id,
                exe: None,
                title: None,
            })
            .collect();

        let resolved = resolve(&windows, &owners);
        assert!(resolved.unresolved.is_empty());
        assert_eq!(resolved.apps.len(), 2);

        let brave = resolved
            .apps
            .iter()
            .find(|app| app.pid == 200)
            .expect("brave");
        assert_eq!(brave.name, "Brave");
        assert_eq!(brave.window_ids, BTreeSet::from([61, 8310]));
    }

    #[test]
    fn falls_back_to_an_unambiguous_owner_name() {
        let owners = HashMap::from([
            (
                1,
                WindowOwner {
                    pid: 100,
                    name: "Ghostty".into(),
                    layer: 0,
                },
            ),
            (
                2,
                WindowOwner {
                    pid: 200,
                    name: "Dup".into(),
                    layer: 0,
                },
            ),
            (
                3,
                WindowOwner {
                    pid: 300,
                    name: "Dup".into(),
                    layer: 0,
                },
            ),
        ]);

        let windows = vec![
            StateWindow {
                id: 99,
                exe: Some("Ghostty".into()),
                title: None,
            },
            StateWindow {
                id: 98,
                exe: Some("Dup".into()),
                title: None,
            },
            StateWindow {
                id: 97,
                exe: Some("Nope".into()),
                title: None,
            },
        ];

        let resolved = resolve(&windows, &owners);
        assert_eq!(resolved.apps.len(), 1);
        assert_eq!(resolved.apps[0].pid, 100);
        assert_eq!(resolved.apps[0].resolution, Resolution::OwnerName);
        assert_eq!(
            resolved.unresolved.len(),
            2,
            "ambiguous and unknown names stay unresolved"
        );
    }

    #[test]
    fn extracts_pids_from_window_manager_events() {
        let event: Value = serde_json::from_str(
            r#"{"type":"FocusChange","content":[{"type":"Accessibility","content":"AXMainWindowChanged"},20934,3395]}"#,
        )
        .expect("json");
        assert_eq!(event_pid(&event), Some(20934));

        let socket: Value =
            serde_json::from_str(r#"{"type":"FocusWindow","content":"Left"}"#).expect("json");
        assert_eq!(event_pid(&socket), None);

        let space: Value = serde_json::from_str(
            r#"{"type":"Show","content":[{"type":"AppKitWorkspace","content":"x"},-1]}"#,
        )
        .expect("json");
        assert_eq!(event_pid(&space), None, "negative pids are not processes");
    }

    #[test]
    fn tracker_reports_each_process_once() {
        let owners = HashMap::from([
            (
                1,
                WindowOwner {
                    pid: 100,
                    name: "A".into(),
                    layer: 0,
                },
            ),
            (
                2,
                WindowOwner {
                    pid: 100,
                    name: "A".into(),
                    layer: 0,
                },
            ),
            (
                3,
                WindowOwner {
                    pid: 200,
                    name: "B".into(),
                    layer: 0,
                },
            ),
        ]);
        let window = |id| StateWindow {
            id,
            exe: None,
            title: None,
        };

        let mut tracker = AppTracker::default();
        let fresh = tracker.observe(resolve(&[window(1), window(3)], &owners), "state");
        assert_eq!(fresh.len(), 2);

        // the same state again discovers nothing new, but a new window of a known app is merged
        let fresh = tracker.observe(
            resolve(&[window(1), window(2), window(3)], &owners),
            "state",
        );
        assert!(fresh.is_empty());
        let records = tracker.records();
        assert_eq!(
            records
                .iter()
                .find(|app| app.pid == 100)
                .expect("a")
                .window_ids,
            vec![1, 2]
        );

        assert!(tracker.observe_pid(100, "A".into(), "event").is_none());
        assert!(tracker.observe_pid(300, "C".into(), "event").is_some());
        assert!(tracker.contains(300));
    }

    #[test]
    fn unresolved_windows_are_reported_until_they_resolve() {
        let mut tracker = AppTracker::default();
        let window = StateWindow {
            id: 7,
            exe: Some("Ghost".into()),
            title: None,
        };
        tracker.observe(
            resolve(std::slice::from_ref(&window), &HashMap::new()),
            "state",
        );
        assert_eq!(tracker.unresolved().len(), 1);

        let owners = HashMap::from([(
            7,
            WindowOwner {
                pid: 5,
                name: "Ghost".into(),
                layer: 0,
            },
        )]);
        tracker.observe(resolve(std::slice::from_ref(&window), &owners), "state");
        assert!(tracker.unresolved().is_empty());
    }

    #[test]
    fn gui_apps_are_layer_zero_owners() {
        let owners = HashMap::from([
            (
                1,
                WindowOwner {
                    pid: 10,
                    name: "App".into(),
                    layer: 0,
                },
            ),
            (
                2,
                WindowOwner {
                    pid: 20,
                    name: "Dock".into(),
                    layer: 20,
                },
            ),
        ]);
        let gui = gui_app_pids(&owners);
        assert_eq!(gui.len(), 1);
        assert_eq!(gui.get(&10).map(String::as_str), Some("App"));
    }

    /// Against the real window server: the call must work and see windows. Sandboxed builds may
    /// have no window server to ask, in which case there is nothing to assert.
    #[test]
    fn window_server_listing_works() {
        let owners = window_owners();
        if owners.is_empty() {
            eprintln!("skipping: no window server is reachable from this environment");
            return;
        }

        assert!(owners.values().all(|owner| owner.pid > 0));
    }
}
