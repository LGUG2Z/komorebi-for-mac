//! komorebi's `State`, as a model that can be asked questions.
//!
//! Every subscriber notification carries the whole `State` serialized as JSON, and the recorder
//! stores each distinct one. The JSON is read tolerantly into a [`Snapshot`]: unknown fields are
//! ignored and missing ones default, so a komorebi newer than this recorder still reads.
//!
//! Two facts about it that matter when reading it:
//!
//! - A window's `rect` is **not** komorebi's bookkeeping. It is read live from the accessibility
//!   API at the moment the state is serialized, so it says where the window *really* was.
//!   Comparing it with where komorebi's layout says it should be (`layout_rects`) finds windows
//!   that are in the wrong place.
//! - komorebi's `Rect` is `{left, top, right, bottom}` where `right` is the **width** and `bottom`
//!   the **height**, in global display points. [`Rect`] uses honest names.

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Rect {
    pub left: i64,
    pub top: i64,
    pub width: i64,
    pub height: i64,
}

impl Rect {
    #[must_use]
    pub fn right_edge(&self) -> i64 {
        self.left + self.width
    }

    #[must_use]
    pub fn bottom_edge(&self) -> i64 {
        self.top + self.height
    }

    /// The area two rectangles share.
    #[must_use]
    pub fn overlap_area(&self, other: &Rect) -> i64 {
        let width = self.right_edge().min(other.right_edge()) - self.left.max(other.left);
        let height = self.bottom_edge().min(other.bottom_edge()) - self.top.max(other.top);
        if width <= 0 || height <= 0 {
            0
        } else {
            width * height
        }
    }

    #[must_use]
    pub fn area(&self) -> i64 {
        self.width.max(0) * self.height.max(0)
    }

    /// The largest difference in any of the four numbers.
    #[must_use]
    pub fn max_delta(&self, other: &Rect) -> i64 {
        [
            (self.left - other.left).abs(),
            (self.top - other.top).abs(),
            (self.width - other.width).abs(),
            (self.height - other.height).abs(),
        ]
        .into_iter()
        .max()
        .unwrap_or(0)
    }

    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{}x{} at ({}, {})",
            self.width, self.height, self.left, self.top
        )
    }

    fn from_value(value: &Value) -> Option<Self> {
        let number = |key: &str| {
            let field = value.get(key)?;
            field
                .as_i64()
                .or_else(|| field.as_f64().map(|float| float.round() as i64))
        };

        Some(Self {
            left: number("left")?,
            top: number("top")?,
            // komorebi's `right` and `bottom` are a width and a height
            width: number("right")?,
            height: number("bottom")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Window {
    pub id: u64,
    pub title: String,
    pub exe: String,
    /// Where the window really was when the state was serialized (a live accessibility read).
    pub rect: Option<Rect>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Container {
    pub id: String,
    pub windows: Vec<Window>,
    /// Which of `windows` is in front.
    pub focused: usize,
    pub locked: bool,
}

impl Container {
    #[must_use]
    pub fn focused_window(&self) -> Option<&Window> {
        self.windows.get(self.focused)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Workspace {
    pub name: String,
    pub containers: Vec<Container>,
    pub focused: usize,
    /// A container filling the workspace on its own.
    pub monocle: Option<Container>,
    pub maximized: Option<Window>,
    pub floating: Vec<Window>,
    pub floating_focused: usize,
    /// `Tiling` or `Floating`.
    pub layer: String,
    pub layout: String,
    /// Where komorebi's last layout pass put each container, by container index.
    pub layout_rects: Vec<Rect>,
    pub tiling: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Monitor {
    /// The display id: the same number as the `--display` of the recording.
    pub id: u64,
    pub device: String,
    pub size: Rect,
    pub work_area: Rect,
    pub workspaces: Vec<Workspace>,
    pub focused: usize,
    /// `BottomLeft` or `BottomRight`: the corner komorebi hides windows in.
    pub hiding_position: String,
}

impl Monitor {
    #[must_use]
    pub fn focused_workspace(&self) -> Option<&Workspace> {
        self.workspaces.get(self.focused)
    }
}

/// Where a window is in the tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Location {
    pub monitor_id: u64,
    pub workspace: String,
    pub place: Place,
    /// Whether that workspace is the one showing on its monitor.
    pub workspace_visible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Place {
    Container { index: usize, tab: usize },
    Monocle { tab: usize },
    Maximized,
    Floating { index: usize },
}

impl Place {
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Container { index, tab } if *tab > 0 => format!("container {index} tab {tab}"),
            Self::Container { index, .. } => format!("container {index}"),
            Self::Monocle { .. } => "monocle".to_string(),
            Self::Maximized => "maximized".to_string(),
            Self::Floating { index } => format!("floating {index}"),
        }
    }
}

impl Location {
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "monitor {} workspace {} ({})",
            self.monitor_id,
            self.workspace,
            self.place.describe()
        )
    }
}

/// What has the keyboard focus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Focus {
    pub monitor_id: u64,
    pub workspace: String,
    pub window_id: Option<u64>,
    pub place: Option<Place>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Snapshot {
    pub monitors: Vec<Monitor>,
    pub focused_monitor: usize,
    pub paused: bool,
}

fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(|ring| ring.get("elements"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

fn focused(value: &Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(|ring| ring.get("focused"))
        .and_then(Value::as_u64)
        .map_or(0, |index| index as usize)
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn window(value: &Value) -> Option<Window> {
    let id = value.get("id")?.as_u64()?;
    let details = value.get("details").unwrap_or(value);
    Some(Window {
        id,
        title: text(details, "title"),
        exe: text(details, "exe"),
        rect: value.get("rect").and_then(Rect::from_value),
    })
}

fn container(value: &Value) -> Container {
    Container {
        id: text(value, "id"),
        windows: array(value, "windows").iter().filter_map(window).collect(),
        focused: focused(value, "windows"),
        locked: value
            .get("locked")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// `{"Default": "Columns"}` or `"BSP"` or `{"Custom": ...}` as a short name.
fn layout_name(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(name)) => name.clone(),
        Some(Value::Object(object)) => {
            object
                .iter()
                .next()
                .map_or_else(String::new, |(key, inner)| match inner.as_str() {
                    Some(name) => name.to_string(),
                    None => key.clone(),
                })
        }
        _ => String::new(),
    }
}

fn workspace(value: &Value) -> Workspace {
    Workspace {
        name: text(value, "name"),
        containers: array(value, "containers").iter().map(container).collect(),
        focused: focused(value, "containers"),
        monocle: value
            .get("monocle_container")
            .filter(|monocle| monocle.is_object())
            .map(container),
        maximized: value.get("maximized_window").and_then(window),
        floating: array(value, "floating_windows")
            .iter()
            .filter_map(window)
            .collect(),
        floating_focused: focused(value, "floating_windows"),
        layer: text(value, "layer"),
        layout: layout_name(value.get("layout")),
        layout_rects: value
            .get("latest_layout")
            .and_then(Value::as_array)
            .map(|rects| rects.iter().filter_map(Rect::from_value).collect())
            .unwrap_or_default(),
        tiling: value.get("tile").and_then(Value::as_bool).unwrap_or(true),
    }
}

fn monitor(value: &Value) -> Monitor {
    let rect = |key: &str| {
        value.get(key).and_then(Rect::from_value).unwrap_or(Rect {
            left: 0,
            top: 0,
            width: 0,
            height: 0,
        })
    };

    Monitor {
        id: value.get("id").and_then(Value::as_u64).unwrap_or(0),
        device: text(value, "device"),
        size: rect("size"),
        work_area: rect("work_area_size"),
        workspaces: array(value, "workspaces").iter().map(workspace).collect(),
        focused: focused(value, "workspaces"),
        hiding_position: text(value, "window_hiding_position"),
    }
}

impl Snapshot {
    /// Read komorebi's `State` JSON.
    #[must_use]
    pub fn from_value(state: &Value) -> Self {
        Self {
            monitors: array(state, "monitors").iter().map(monitor).collect(),
            focused_monitor: focused(state, "monitors"),
            paused: state
                .get("is_paused")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }
    }

    /// Every window with where it is.
    #[must_use]
    pub fn windows(&self) -> Vec<(Location, &Window)> {
        let mut found = Vec::new();
        for monitor in &self.monitors {
            for (index, workspace) in monitor.workspaces.iter().enumerate() {
                let visible = index == monitor.focused;
                let at = |place: Place| Location {
                    monitor_id: monitor.id,
                    workspace: workspace.name.clone(),
                    place,
                    workspace_visible: visible,
                };

                for (container_index, container) in workspace.containers.iter().enumerate() {
                    for (tab, window) in container.windows.iter().enumerate() {
                        found.push((
                            at(Place::Container {
                                index: container_index,
                                tab,
                            }),
                            window,
                        ));
                    }
                }
                if let Some(monocle) = &workspace.monocle {
                    for (tab, window) in monocle.windows.iter().enumerate() {
                        found.push((at(Place::Monocle { tab }), window));
                    }
                }
                if let Some(window) = &workspace.maximized {
                    found.push((at(Place::Maximized), window));
                }
                for (floating_index, window) in workspace.floating.iter().enumerate() {
                    found.push((
                        at(Place::Floating {
                            index: floating_index,
                        }),
                        window,
                    ));
                }
            }
        }

        found
    }

    /// The first place a window with this id is.
    #[must_use]
    pub fn find(&self, window_id: u64) -> Option<(Location, &Window)> {
        self.windows()
            .into_iter()
            .find(|(_, window)| window.id == window_id)
    }

    /// What has focus: the focused monitor's focused workspace's focused window, where a floating
    /// layer beats the tiled containers and a monocle container beats the rest.
    #[must_use]
    pub fn focus(&self) -> Option<Focus> {
        let monitor = self
            .monitors
            .get(self.focused_monitor)
            .or_else(|| self.monitors.first())?;
        let workspace = monitor.focused_workspace()?;

        let (window, place) = if workspace.layer == "Floating" && !workspace.floating.is_empty() {
            let index = workspace
                .floating_focused
                .min(workspace.floating.len().saturating_sub(1));
            (
                workspace.floating.get(index),
                Some(Place::Floating { index }),
            )
        } else if let Some(monocle) = &workspace.monocle {
            (
                monocle.focused_window(),
                Some(Place::Monocle {
                    tab: monocle.focused,
                }),
            )
        } else if let Some(container) = workspace.containers.get(workspace.focused) {
            (
                container.focused_window(),
                Some(Place::Container {
                    index: workspace.focused,
                    tab: container.focused,
                }),
            )
        } else {
            (None, None)
        };

        Some(Focus {
            monitor_id: monitor.id,
            workspace: workspace.name.clone(),
            window_id: window.map(|window| window.id),
            place,
        })
    }

    /// Whether `rect` is where komorebi parks a window it has hidden on `monitor`: with a
    /// one-point sliver still inside the display, so the window stays "on screen" to macOS. The
    /// vertical position is not checked, because macOS pulls a window up to keep its title bar
    /// reachable.
    #[must_use]
    pub fn is_hidden_position(rect: &Rect, monitor: &Monitor) -> bool {
        let tolerance = 2;
        let bottom_right = monitor.hiding_position == "BottomRight";
        if bottom_right {
            (rect.left - (monitor.size.left + monitor.size.width - 1)).abs() <= tolerance
        } else {
            (rect.right_edge() - (monitor.size.left + 1)).abs() <= tolerance
        }
    }

    /// The monitor whose area holds most of `rect`.
    #[must_use]
    pub fn monitor_under(&self, rect: &Rect) -> Option<&Monitor> {
        self.monitors
            .iter()
            .map(|monitor| (monitor.size.overlap_area(rect), monitor))
            .filter(|(area, _)| *area > 0)
            .max_by_key(|(area, _)| *area)
            .map(|(_, monitor)| monitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Snapshot {
        let state: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/state.json")).expect("fixture");
        Snapshot::from_value(&state)
    }

    #[test]
    fn a_real_state_reads_into_monitors_workspaces_and_windows() {
        let snapshot = fixture();

        assert_eq!(snapshot.monitors.len(), 2);
        assert_eq!(snapshot.monitors[0].id, 1);
        assert_eq!(snapshot.monitors[0].device, "Built-in Retina Display");
        assert_eq!(snapshot.monitors[1].workspaces.len(), 10);
        assert!(!snapshot.paused);

        let windows = snapshot.windows();
        assert!(windows.len() > 15);
        let (location, slack) = snapshot.find(59).expect("window 59");
        assert_eq!(slack.exe, "Slack");
        assert_eq!(slack.title, "window 59");
        assert_eq!(location.monitor_id, 3);
        assert_eq!(location.workspace, "one");
        assert_eq!(location.place, Place::Container { index: 0, tab: 0 });
    }

    #[test]
    fn komorebis_right_and_bottom_are_read_as_width_and_height() {
        let snapshot = fixture();
        let layout = &snapshot.monitors[1].workspaces[0].layout_rects;
        // {"left": 10, "top": 10, "right": 1686, "bottom": 1420}
        assert_eq!(
            layout[0],
            Rect {
                left: 10,
                top: 10,
                width: 1686,
                height: 1420
            }
        );
        assert_eq!(layout[0].right_edge(), 1696);
    }

    #[test]
    fn layouts_have_short_names() {
        assert_eq!(fixture().monitors[1].workspaces[0].layout, "Columns");
        assert_eq!(layout_name(Some(&serde_json::json!("BSP"))), "BSP");
        assert_eq!(layout_name(None), "");
    }

    #[test]
    fn hidden_windows_sit_on_the_sliver_and_visible_ones_do_not() {
        let snapshot = fixture();
        let monitor = &snapshot.monitors[1];
        let (_, hidden) = snapshot
            .find(59)
            .expect("a window on a workspace that is not showing");
        assert!(Snapshot::is_hidden_position(
            hidden.rect.as_ref().expect("rect"),
            monitor
        ));

        let visible = Rect {
            left: 10,
            top: 10,
            width: 1686,
            height: 1420,
        };
        assert!(!Snapshot::is_hidden_position(&visible, monitor));
    }

    #[test]
    fn focus_follows_the_focused_monitor_workspace_and_container() {
        let focus = fixture().focus().expect("focus");
        // the second monitor (display 3) is focused, on workspace "ten", at its third container
        assert_eq!(focus.monitor_id, 3);
        assert_eq!(focus.workspace, "ten");
        assert_eq!(focus.window_id, Some(8550));
        assert_eq!(focus.place, Some(Place::Container { index: 2, tab: 0 }));
    }

    fn workspace_state(extra: Value) -> Snapshot {
        let mut workspace = serde_json::json!({
            "name": "w", "layer": "Tiling",
            "containers": {"elements": [
                {"id": "a", "windows": {"elements": [{"id": 1, "rect": null}], "focused": 0}},
                {"id": "b", "windows": {"elements": [{"id": 2}], "focused": 0}}
            ], "focused": 1},
            "floating_windows": {"elements": [{"id": 3}], "focused": 0},
        });
        for (key, value) in extra.as_object().expect("object") {
            workspace[key] = value.clone();
        }
        Snapshot::from_value(&serde_json::json!({
            "monitors": {"elements": [{"id": 1, "workspaces": {"elements": [workspace], "focused": 0}}], "focused": 0}
        }))
    }

    #[test]
    fn a_monocle_container_beats_the_containers_and_a_floating_layer_beats_both() {
        let plain = workspace_state(serde_json::json!({}));
        assert_eq!(plain.focus().expect("focus").window_id, Some(2));

        let monocle = workspace_state(serde_json::json!({
            "monocle_container": {"id": "m", "windows": {"elements": [{"id": 9}], "focused": 0}}
        }));
        assert_eq!(monocle.focus().expect("focus").window_id, Some(9));

        let floating = workspace_state(serde_json::json!({
            "layer": "Floating",
            "monocle_container": {"id": "m", "windows": {"elements": [{"id": 9}], "focused": 0}}
        }));
        assert_eq!(floating.focus().expect("focus").window_id, Some(3));
    }

    #[test]
    fn missing_and_unknown_fields_do_not_break_reading() {
        let snapshot = Snapshot::from_value(&serde_json::json!({"brand_new": 1}));
        assert!(snapshot.monitors.is_empty());
        assert!(snapshot.focus().is_none());

        let odd = Snapshot::from_value(&serde_json::json!({
            "monitors": {"elements": [{"id": 1, "future_field": [1, 2], "workspaces": {"elements": [{"name": "x", "monocle_container": null}]}}]}
        }));
        assert_eq!(odd.monitors[0].workspaces[0].name, "x");
        assert!(odd.monitors[0].workspaces[0].monocle.is_none());
    }

    #[test]
    fn a_window_without_a_rect_has_none_and_overlap_is_computed() {
        let snapshot = workspace_state(serde_json::json!({}));
        assert!(snapshot.find(1).expect("window").1.rect.is_none());

        let a = Rect {
            left: 0,
            top: 0,
            width: 10,
            height: 10,
        };
        let b = Rect {
            left: 5,
            top: 5,
            width: 10,
            height: 10,
        };
        assert_eq!(a.overlap_area(&b), 25);
        assert_eq!(
            a.overlap_area(&Rect {
                left: 20,
                top: 0,
                width: 5,
                height: 5
            }),
            0
        );
        assert_eq!(a.max_delta(&b), 5);
    }
}
