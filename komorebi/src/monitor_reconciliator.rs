use crate::core::rect::Rect;
use crate::core_graphics::CoreGraphicsApi;
use crate::window_manager::WindowManager;
use crossbeam_channel::Receiver;
use crossbeam_channel::Sender;
use objc2_core_graphics::CGDirectDisplayID;
use parking_lot::Mutex;
use serde::Deserialize;
use serde::Serialize;
use std::sync::Arc;
use std::sync::OnceLock;
use strum::Display;

#[derive(Debug, Copy, Clone, Serialize, Deserialize, Display)]

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
        let mut wm = wm.lock();

        match notification {
            MonitorNotification::Resize(display_id) => {
                tracing::debug!("handling resize notification");
                let new_size = CoreGraphicsApi::display_bounds(display_id);
                for monitor in wm.monitors_mut() {
                    let mut should_update = false;

                    if Rect::from(new_size) != monitor.size {
                        // need to do this because we can't call NSScreen from a background thread
                        monitor.work_area_size = calculate_scaled_work_area(
                            &Rect::from(new_size),
                            &monitor.size,
                            &monitor.work_area_size,
                        );
                        monitor.size = Rect::from(new_size);
                        should_update = true;
                    }

                    if should_update {
                        tracing::info!("updated size and work area for monitor: {}", monitor.id);
                        monitor.update_focused_workspace(None)?;
                    }
                }
            }
        }
    }

    Ok(())
}

fn calculate_scaled_work_area(
    new_size: &Rect,
    monitor_size: &Rect,
    monitor_work_area_size: &Rect,
) -> Rect {
    let scale_x = new_size.right as f64 / monitor_size.right as f64;
    let scale_y = new_size.bottom as f64 / monitor_size.bottom as f64;

    let top_offset = monitor_work_area_size.top - monitor_size.top;
    let bottom_offset = monitor_size.bottom - monitor_work_area_size.bottom;
    let left_offset = monitor_work_area_size.left - monitor_size.left;
    let right_offset = monitor_size.right - monitor_work_area_size.right;

    let scaled_top_offset = (top_offset as f64 * scale_y).round() as i32;
    let scaled_bottom_offset = (bottom_offset as f64 * scale_y).round() as i32;
    let scaled_left_offset = (left_offset as f64 * scale_x).round() as i32;
    let scaled_right_offset = (right_offset as f64 * scale_x).round() as i32;

    Rect {
        left: new_size.left + scaled_left_offset,
        top: new_size.top + scaled_top_offset,
        right: new_size.right - scaled_right_offset,
        bottom: new_size.bottom - scaled_bottom_offset,
    }
}
