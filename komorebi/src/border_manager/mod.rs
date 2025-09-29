mod border;
mod ns_window;

use crate::AccessibilityUiElement;
use crate::CoreFoundationRunLoop;
use crate::accessibility::AccessibilityApi;
use crate::border_manager::border::Border;
use crate::core::WindowKind;
use crate::macos_api::MacosApi;
use crate::ring::Ring;
use crate::window_manager::WindowManager;
use crate::workspace::Workspace;
use crate::workspace::WorkspaceLayer;
use color_eyre::eyre;
use crossbeam_channel::Receiver;
use crossbeam_channel::Sender;
use crossbeam_utils::atomic::AtomicConsume;
use dispatch2::DispatchQueue;
use komorebi_themes::colour::Colour;
use komorebi_themes::colour::Rgb;
use lazy_static::lazy_static;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicI32;
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering;

pub static BORDER_WIDTH: AtomicI32 = AtomicI32::new(6);
pub static BORDER_OFFSET: AtomicI32 = AtomicI32::new(5);
pub static BORDER_RADIUS: AtomicI32 = AtomicI32::new(10);
pub static BORDER_ENABLED: AtomicBool = AtomicBool::new(true);

lazy_static! {
    pub static ref FOCUSED: AtomicU32 =
        AtomicU32::new(u32::from(Colour::Rgb(Rgb::new(66, 165, 245))));
    pub static ref UNFOCUSED: AtomicU32 =
        AtomicU32::new(u32::from(Colour::Rgb(Rgb::new(128, 128, 128))));
    pub static ref UNFOCUSED_LOCKED: AtomicU32 =
        AtomicU32::new(u32::from(Colour::Rgb(Rgb::new(158, 8, 8))));
    pub static ref MONOCLE: AtomicU32 =
        AtomicU32::new(u32::from(Colour::Rgb(Rgb::new(255, 51, 153))));
    pub static ref STACK: AtomicU32 = AtomicU32::new(u32::from(Colour::Rgb(Rgb::new(0, 165, 66))));
    pub static ref FLOATING: AtomicU32 =
        AtomicU32::new(u32::from(Colour::Rgb(Rgb::new(245, 245, 165))));
}

lazy_static! {
    static ref BORDER_STATE: Mutex<HashMap<String, Box<Border>>> = Mutex::new(HashMap::new());
    static ref WINDOWS_BORDERS: Mutex<HashMap<u32, String>> = Mutex::new(HashMap::new());
}

pub enum Notification {
    Update(Option<AccessibilityUiElement>, Option<u32>),
    ForceUpdate,
}

static CHANNEL: OnceLock<(Sender<Notification>, Receiver<Notification>)> = OnceLock::new();

pub fn channel() -> &'static (Sender<Notification>, Receiver<Notification>) {
    CHANNEL.get_or_init(|| crossbeam_channel::bounded(50))
}

fn event_tx() -> Sender<Notification> {
    channel().0.clone()
}

fn event_rx() -> Receiver<Notification> {
    channel().1.clone()
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct BorderInfo {
    pub border_id: String,
    pub window_kind: WindowKind,
}

pub fn window_border(window_id: u32) -> Option<BorderInfo> {
    let id = WINDOWS_BORDERS.lock().get(&window_id)?.clone();
    BORDER_STATE.lock().get(&id).map(|b| BorderInfo {
        border_id: b.id.clone(),
        window_kind: b.window_kind,
    })
}

pub fn send_notification(element: Option<AccessibilityUiElement>, window_id: Option<u32>) {
    if event_tx()
        .try_send(Notification::Update(element, window_id))
        .is_err()
    {
        tracing::warn!("channel is full; dropping notification")
    }
}

pub fn destroy_all_borders() -> eyre::Result<()> {
    let mut borders = BORDER_STATE.lock();
    tracing::info!(
        "purging known borders: {:?}",
        borders.iter().map(|b| b.1.id.clone()).collect::<Vec<_>>()
    );

    for (_, border) in borders.drain() {
        let _ = destroy_border(border);
    }

    drop(borders);

    WINDOWS_BORDERS.lock().clear();

    Ok(())
}

fn window_kind_colour(focus_kind: WindowKind) -> u32 {
    match focus_kind {
        WindowKind::Unfocused => UNFOCUSED.load(Ordering::Relaxed),
        WindowKind::UnfocusedLocked => UNFOCUSED_LOCKED.load(Ordering::Relaxed),
        WindowKind::Single => FOCUSED.load(Ordering::Relaxed),
        WindowKind::Stack => STACK.load(Ordering::Relaxed),
        WindowKind::Monocle => MONOCLE.load(Ordering::Relaxed),
        WindowKind::Floating => FLOATING.load(Ordering::Relaxed),
    }
}

fn remove_borders(
    borders: &mut HashMap<String, Box<Border>>,
    windows_borders: &mut HashMap<u32, String>,
    monitor_idx: usize,
    condition: impl Fn(&String, &Border) -> bool,
) -> color_eyre::Result<()> {
    let mut to_remove = vec![];
    for (id, border) in borders.iter() {
        // if border is on this monitor
        if border.monitor_idx.is_some_and(|idx| idx == monitor_idx)
            // and the condition applies
            && condition(id, border)
        {
            // we mark it to be removed
            to_remove.push(id.clone());
        }
    }

    for id in &to_remove {
        remove_border(id, borders, windows_borders)?;
    }

    Ok(())
}

fn remove_border(
    id: &str,
    borders: &mut HashMap<String, Box<Border>>,
    windows_borders: &mut HashMap<u32, String>,
) -> color_eyre::Result<()> {
    if let Some(removed_border) = borders.remove(id) {
        windows_borders.remove(&removed_border.tracking_window_id);
        destroy_border(removed_border)?;
    }

    Ok(())
}

fn destroy_border(border: Box<Border>) -> color_eyre::Result<()> {
    DispatchQueue::main().exec_sync(|| {
        tracing::info!("invalidating border observer");
        AccessibilityApi::invalidate_observer(&border.observer);
    });

    std::thread::sleep(std::time::Duration::from_millis(10));

    let raw_pointer = Box::into_raw(border);
    unsafe {
        (*raw_pointer).destroy();
    }

    Ok(())
}

pub fn listen_for_notifications(wm: Arc<Mutex<WindowManager>>, run_loop: CoreFoundationRunLoop) {
    std::thread::spawn(move || {
        loop {
            match handle_notifications(wm.clone(), run_loop.clone()) {
                Ok(()) => {
                    tracing::warn!("restarting finished thread");
                }
                Err(error) => {
                    tracing::warn!("restarting failed thread: {}", error);
                }
            }
        }
    });
}

fn handle_notifications(
    wm: Arc<Mutex<WindowManager>>,
    run_loop: CoreFoundationRunLoop,
) -> color_eyre::Result<()> {
    tracing::info!("listening");

    let receiver = event_rx();
    event_tx().send(Notification::Update(None, None))?;

    let mut previous_snapshot = Ring::default();
    let mut previous_pending_move_op = None;
    let mut previous_is_paused = false;
    let mut previous_notification: Option<Notification> = None;
    let mut previous_layer = WorkspaceLayer::default();

    'receiver: for notification in receiver {
        let state = wm.lock();
        let is_paused = state.is_paused;
        let focused_monitor_idx = state.focused_monitor_idx();
        let focused_workspace_idx =
            state.monitors.elements()[focused_monitor_idx].focused_workspace_idx();
        let monitors = state.monitors.clone();
        let pending_move_op = *state.pending_move_op;
        let floating_window_hwnds = state.monitors.elements()[focused_monitor_idx].workspaces()
            [focused_workspace_idx]
            .floating_windows()
            .iter()
            .map(|w| w.id)
            .collect::<Vec<_>>();
        let workspace_layer = state.monitors.elements()[focused_monitor_idx].workspaces()
            [focused_workspace_idx]
            .layer;
        let foreground_window = MacosApi::foreground_window_id().unwrap_or_default();
        let _layer_changed = previous_layer != workspace_layer;
        let _forced_update = matches!(notification, Notification::ForceUpdate);

        drop(state);

        let should_process_notification = match notification {
            Notification::Update(_, notification_window_id) => {
                let mut should_process_notification = true;

                if monitors == previous_snapshot
                    // handle the window dragging edge case
                    && pending_move_op == previous_pending_move_op
                {
                    should_process_notification = false;
                }

                // handle the pause edge case
                if is_paused && !previous_is_paused {
                    should_process_notification = true;
                }

                // handle the unpause edge case
                if previous_is_paused && !is_paused {
                    should_process_notification = true;
                }

                // handle the retile edge case
                if !should_process_notification && BORDER_STATE.lock().is_empty() {
                    should_process_notification = true;
                }

                // when we switch focus to/from a floating window
                let switch_focus_to_from_floating_window = floating_window_hwnds.iter().any(|fw| {
                    // if we switch focus to a floating window
                    fw == &notification_window_id.unwrap_or_default() ||
                            // if there is any floating window with a `WindowKind::Floating` border
                            // that no longer is the foreground window then we need to update that
                            // border.
                            (fw != &foreground_window
                                && window_border(*fw)
                                .is_some_and(|b| b.window_kind == WindowKind::Floating))
                });

                // when the focused window has an `Unfocused` border kind, usually this happens if
                // we focus an admin window and then refocus the previously focused window. For
                // komorebi it will have the same state has before, however the previously focused
                // window changed its border to unfocused so now we need to update it again.
                if !should_process_notification
                    && window_border(notification_window_id.unwrap_or_default())
                        .is_some_and(|b| b.window_kind == WindowKind::Unfocused)
                {
                    should_process_notification = true;
                }

                if !should_process_notification && switch_focus_to_from_floating_window {
                    should_process_notification = true;
                }

                if !should_process_notification
                    && let Some(Notification::Update(_, ref previous_window_id)) =
                        previous_notification
                    && previous_window_id.unwrap_or_default()
                        != notification_window_id.unwrap_or_default()
                {
                    should_process_notification = true;
                }

                should_process_notification
            }
            Notification::ForceUpdate => true,
        };

        if !should_process_notification {
            // tracing::debug!("monitor state matches latest snapshot, skipping notification");
            continue 'receiver;
        }

        let mut borders = BORDER_STATE.lock();
        let mut windows_borders = WINDOWS_BORDERS.lock();

        // If borders are disabled
        if !BORDER_ENABLED.load_consume()
            // Or if the wm is paused
            || is_paused
        {
            // Destroy the borders we know about
            for (_, border) in borders.drain() {
                destroy_border(border)?;
            }

            windows_borders.clear();

            previous_is_paused = is_paused;
            continue 'receiver;
        }

        'monitors: for (monitor_idx, m) in monitors.elements().iter().enumerate() {
            if let Some(ws) = m.focused_workspace() {
                if !ws.tile {
                    remove_borders(&mut borders, &mut windows_borders, monitor_idx, |_, _| true)?;
                    continue 'monitors;
                }

                if let Some(monocle) = &ws.monocle_container
                    && let Some(window) = monocle.focused_window()
                {
                    let focused_window_id =
                        monocle.focused_window().map(|w| w.id).unwrap_or_default();
                    let id = monocle.id.clone();
                    let border = match borders.entry(id.clone()) {
                        Entry::Occupied(entry) => entry.into_mut(),
                        Entry::Vacant(entry) => {
                            if let Ok(border) = Border::create(
                                &monocle.id,
                                window.id,
                                window.application.process_id,
                                window.element.clone(),
                                Some(monitor_idx),
                                run_loop.clone(),
                            ) {
                                entry.insert(border)
                            } else {
                                continue 'monitors;
                            }
                        }
                    };

                    let new_focus_state = if monitor_idx != focused_monitor_idx {
                        WindowKind::Unfocused
                    } else {
                        WindowKind::Monocle
                    };

                    border.window_kind = new_focus_state;

                    // Update the borders tracking_hwnd in case it changed and remove the
                    // old `tracking_hwnd` from `WINDOWS_BORDERS` if needed.
                    if border.tracking_window_id != focused_window_id {
                        if let Some(previous) = windows_borders.get(&border.tracking_window_id) {
                            // Only remove the border from `windows_borders` if it
                            // still corresponds to the same border, if doesn't then
                            // it means it was already updated by another border for
                            // that window and in that case we don't want to remove it.
                            if previous == &id {
                                windows_borders.remove(&border.tracking_window_id);
                            }
                        }
                        border.tracking_window_id = focused_window_id;
                    }

                    // Update the border's monitor idx in case it changed
                    border.monitor_idx = Some(monitor_idx);
                    border.update();

                    windows_borders.insert(focused_window_id, id);

                    let border_id = border.id.clone();

                    if ws.layer == WorkspaceLayer::Floating {
                        handle_floating_borders(
                            &mut borders,
                            &mut windows_borders,
                            ws,
                            monitor_idx,
                            foreground_window,
                            run_loop.clone(),
                        )?;

                        // Remove all borders on this monitor except monocle and floating borders
                        remove_borders(&mut borders, &mut windows_borders, monitor_idx, |_, b| {
                            border_id != b.id
                                && !ws
                                    .floating_windows()
                                    .iter()
                                    .any(|w| w.id == b.tracking_window_id)
                        })?;
                    } else {
                        // Remove all borders on this monitor except monocle
                        remove_borders(&mut borders, &mut windows_borders, monitor_idx, |_, b| {
                            border_id != b.id
                        })?;
                    }
                    continue 'monitors;
                }

                // Collect focused workspace container and floating windows ID's
                let container_and_floating_window_ids = ws
                    .containers()
                    .iter()
                    .map(|c| c.id.clone())
                    .collect::<Vec<_>>();

                remove_borders(&mut borders, &mut windows_borders, monitor_idx, |id, _| {
                    !container_and_floating_window_ids.contains(id)
                })?;

                for (idx, c) in ws.containers().iter().enumerate() {
                    if let Some(window) = c.focused_window() {
                        let id = c.id.clone();

                        let border = match borders.entry(id.clone()) {
                            Entry::Occupied(entry) => entry.into_mut(),
                            Entry::Vacant(entry) => {
                                if let Ok(border) = Border::create(
                                    &c.id,
                                    window.id,
                                    window.application.process_id,
                                    window.element.clone(),
                                    Some(monitor_idx),
                                    run_loop.clone(),
                                ) {
                                    entry.insert(border)
                                } else {
                                    continue 'monitors;
                                }
                            }
                        };

                        let new_focus_state = if idx != ws.focused_container_idx()
                            || monitor_idx != focused_monitor_idx
                            || window.id != foreground_window
                        {
                            if c.locked {
                                WindowKind::UnfocusedLocked
                            } else {
                                WindowKind::Unfocused
                            }
                        } else if c.windows().len() > 1 {
                            WindowKind::Stack
                        } else {
                            WindowKind::Single
                        };

                        border.window_kind = new_focus_state;

                        if border.tracking_window_id != window.id {
                            if let Some(previous) = windows_borders.get(&border.tracking_window_id)
                            {
                                // Only remove the border from `windows_borders` if it
                                // still corresponds to the same border, if doesn't then
                                // it means it was already updated by another border for
                                // that window and in that case we don't want to remove it.
                                if previous == &id {
                                    windows_borders.remove(&border.tracking_window_id);
                                }
                            }
                            border.tracking_window_id = window.id;
                        }

                        border.monitor_idx = Some(monitor_idx);
                        border.update();

                        windows_borders.insert(window.id, id);
                    }

                    handle_floating_borders(
                        &mut borders,
                        &mut windows_borders,
                        ws,
                        monitor_idx,
                        foreground_window,
                        run_loop.clone(),
                    )?;
                }
            }
        }

        previous_snapshot = monitors;
        previous_pending_move_op = pending_move_op;
        previous_is_paused = is_paused;
        previous_notification = Some(notification);
        previous_layer = workspace_layer;
    }

    Ok(())
}

fn handle_floating_borders(
    borders: &mut HashMap<String, Box<Border>>,
    windows_borders: &mut HashMap<u32, String>,
    ws: &Workspace,
    monitor_idx: usize,
    foreground_window: u32,
    run_loop: CoreFoundationRunLoop,
) -> color_eyre::Result<()> {
    for window in ws.floating_windows() {
        let id = window.id.to_string();
        let border = match borders.entry(id.clone()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                if let Ok(border) = Border::create(
                    &window.id.to_string(),
                    window.id,
                    window.application.process_id,
                    window.element.clone(),
                    Some(monitor_idx),
                    run_loop.clone(),
                ) {
                    entry.insert(border)
                } else {
                    return Ok(());
                }
            }
        };

        let new_focus_state = if foreground_window == window.id {
            WindowKind::Floating
        } else {
            WindowKind::Unfocused
        };

        border.window_kind = new_focus_state;
        // Update the border's monitor idx in case it changed
        border.monitor_idx = Some(monitor_idx);

        border.update();

        windows_borders.insert(window.id, id);
    }

    Ok(())
}
