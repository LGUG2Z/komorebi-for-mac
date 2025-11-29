use crate::TABBED_APPLICATIONS;
use crate::accessibility::AccessibilityApi;
use crate::accessibility::error::AccessibilityError;
use crate::border_manager;
use crate::window::Window;
use crate::window_manager::WindowManager;
use crossbeam_channel::Receiver;
use crossbeam_channel::Sender;
use objc2_core_foundation::CFBoolean;
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::OnceLock;

pub enum ReaperNotification {
    InvalidWindow(u32),
    MouseUpKeyUp,
}

static CHANNEL: OnceLock<(Sender<ReaperNotification>, Receiver<ReaperNotification>)> =
    OnceLock::new();

pub fn channel() -> &'static (Sender<ReaperNotification>, Receiver<ReaperNotification>) {
    CHANNEL.get_or_init(|| crossbeam_channel::bounded(50))
}

fn event_tx() -> Sender<ReaperNotification> {
    channel().0.clone()
}

fn event_rx() -> Receiver<ReaperNotification> {
    channel().1.clone()
}

pub fn send_notification(notification: ReaperNotification) {
    if event_tx().try_send(notification).is_err() {
        tracing::warn!("channel is full; dropping notification")
    }
}

pub fn listen_for_notifications(wm: Arc<Mutex<WindowManager>>) {
    std::thread::spawn(move || {
        loop {
            match handle_notifications(wm.clone()) {
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

fn handle_notifications(wm: Arc<Mutex<WindowManager>>) -> color_eyre::Result<()> {
    tracing::info!("listening");

    let receiver = event_rx();

    'notification: for notification in receiver {
        let mut wm = wm.lock();
        if wm.is_paused {
            tracing::debug!("skipping reaper notification while wm is paused");
            continue 'notification;
        }

        match notification {
            ReaperNotification::InvalidWindow(window_id) => {
                let mut should_update = false;
                for monitor in wm.monitors_mut() {
                    for workspace in monitor.workspaces_mut() {
                        for container in workspace.containers_mut() {
                            container.windows_mut().retain(|w| {
                                if w.id == window_id {
                                    should_update = true;
                                    tracing::info!("reaping window: {window_id}");
                                }

                                w.id != window_id
                            });
                        }

                        if let Some(container) = &mut workspace.monocle_container {
                            container.windows_mut().retain(|w| {
                                if w.id == window_id {
                                    should_update = true;
                                    tracing::info!("reaping window: {window_id}");
                                }

                                w.id != window_id
                            });

                            if container.windows().is_empty() {
                                workspace.monocle_container = None;
                                workspace.monocle_container_restore_idx = None;
                            }
                        }

                        workspace.floating_windows_mut().retain(|w| {
                            if w.id == window_id {
                                should_update = true;
                                tracing::info!("reaping window: {window_id}");
                            }

                            w.id != window_id
                        });
                    }
                }

                if should_update {
                    // If an invalid window was cleaned up, we update the workspace
                    wm.update_focused_workspace(false, false)?;
                    border_manager::send_notification(None, Some(window_id));
                }
            }
            ReaperNotification::MouseUpKeyUp => {
                // this first one will do a "nothing" update to check if any windows failed
                // to have their positions set - the failures will trigger an InvalidWindow
                // notification
                // TODO: maybe replace this with something a bit more lightweight
                wm.update_focused_workspace(false, false)?;
            }
        }
    }

    Ok(())
}

pub fn notify_on_error(
    window: &Window,
    result: Result<(), AccessibilityError>,
) -> Result<(), AccessibilityError> {
    if let Err(_error) = &result {
        let mut should_reap = true;
        let tabbed_applications = TABBED_APPLICATIONS.lock();
        if tabbed_applications.contains(&window.application.name().unwrap_or_default())
            && window.is_valid()
        {
            should_reap = false;
        }

        if let Some(is_fullscreen) =
            AccessibilityApi::copy_attribute_value::<CFBoolean>(&window.element, "AXFullScreen")
            && is_fullscreen.as_bool()
        {
            tracing::debug!(
                "skipping reap for fullscreen window {} during position update failure",
                window.id
            );
            should_reap = false;
        }

        // calibre does some weird stuff when pressing on the menu options which cause
        // layout updates to fail, I guess we should also do a generic is_valid check here
        if window.is_valid() {
            should_reap = false;
        }

        if should_reap {
            send_notification(ReaperNotification::InvalidWindow(window.id));
        }
    }

    result
}
