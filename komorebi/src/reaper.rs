use crate::TABBED_APPLICATIONS;
use crate::accessibility::error::AccessibilityError;
use crate::window::Window;
use crate::window_manager::WindowManager;
use crossbeam_channel::Receiver;
use crossbeam_channel::Sender;
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

    for notification in receiver {
        let mut wm = wm.lock();

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

                        workspace.floating_windows_mut().retain(|w| {
                            if w.id == window_id {
                                should_update = true;
                                tracing::info!("reaping window: {window_id}");
                            }

                            w.id != window_id
                        });
                    }
                }

                // If an invalid window was cleaned up, we update the workspace
                wm.update_focused_workspace(false, false)?;
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

        if should_reap {
            send_notification(ReaperNotification::InvalidWindow(window.id));
        }
    }

    result
}
