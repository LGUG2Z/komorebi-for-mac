use crate::window_manager_event::WindowManagerEvent;
use crossbeam_channel::Receiver;
use crossbeam_channel::Sender;
use std::sync::OnceLock;

static CHANNEL: OnceLock<(Sender<WindowManagerEvent>, Receiver<WindowManagerEvent>)> =
    OnceLock::new();

fn channel() -> &'static (Sender<WindowManagerEvent>, Receiver<WindowManagerEvent>) {
    CHANNEL.get_or_init(|| crossbeam_channel::bounded(20))
}

fn event_tx() -> Sender<WindowManagerEvent> {
    channel().0.clone()
}

pub fn event_rx() -> Receiver<WindowManagerEvent> {
    channel().1.clone()
}

pub fn send_notification(notification: WindowManagerEvent) {
    if event_tx().try_send(notification).is_err() {
        tracing::warn!("channel is full; dropping notification")
    }
}
