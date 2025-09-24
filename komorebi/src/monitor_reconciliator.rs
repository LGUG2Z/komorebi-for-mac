use crate::Notification;
use crate::NotificationEvent;
use crate::UPDATE_MONITOR_WORK_AREAS;
use crate::notify_subscribers;
use crate::state::State;
use crate::window_manager::WindowManager;
use crossbeam_channel::Receiver;
use crossbeam_channel::Sender;
use objc2_core_graphics::CGDirectDisplayID;
use parking_lot::Mutex;
use serde::Deserialize;
use serde::Serialize;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use strum::Display;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, Display)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum MonitorNotification {
    Resize(CGDirectDisplayID),
}

static CHANNEL: OnceLock<(Sender<MonitorNotification>, Receiver<MonitorNotification>)> =
    OnceLock::new();
pub fn channel() -> &'static (Sender<MonitorNotification>, Receiver<MonitorNotification>) {
    CHANNEL.get_or_init(|| crossbeam_channel::bounded(20))
}

fn event_tx() -> Sender<MonitorNotification> {
    channel().0.clone()
}

fn event_rx() -> Receiver<MonitorNotification> {
    channel().1.clone()
}

pub fn send_notification(notification: MonitorNotification) {
    if event_tx().try_send(notification).is_err() {
        tracing::warn!("channel is full; dropping notification")
    }
}

pub fn listen_for_notifications(wm: Arc<Mutex<WindowManager>>) -> color_eyre::Result<()> {
    std::thread::spawn(move || {
        loop {
            match handle_notifications(wm.clone()) {
                Ok(()) => {
                    tracing::warn!("restarting finished thread");
                }
                Err(error) => {
                    if cfg!(debug_assertions) {
                        tracing::error!("restarting failed thread: {:?}", error)
                    } else {
                        tracing::error!("restarting failed thread: {}", error)
                    }
                }
            }
        }
    });

    Ok(())
}

pub fn handle_notifications(wm: Arc<Mutex<WindowManager>>) -> color_eyre::Result<()> {
    tracing::info!("listening");

    let receiver = event_rx();

    for notification in receiver {
        let wm = wm.lock();

        let initial_state = State::from(wm.as_ref());

        match notification {
            MonitorNotification::Resize(_display_id) => {
                tracing::debug!("handling resize notification");
                UPDATE_MONITOR_WORK_AREAS.store(true, Ordering::Relaxed);
            }
        }

        notify_subscribers(
            Notification {
                event: NotificationEvent::Monitor(notification),
                state: wm.as_ref().into(),
            },
            initial_state.has_been_modified(&wm),
        )?;
    }

    Ok(())
}
