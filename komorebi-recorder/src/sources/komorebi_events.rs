//! komorebi's subscriber-socket notifications: every window manager event together with the full
//! `State` it produced.
//!
//! The daemon pushes each notification as one JSON blob over a fresh unix-socket connection to a
//! listener we bind, and connects *synchronously* per subscriber while holding its own locks, so
//! a slow subscriber stalls komorebi. The accept thread therefore does the bare minimum (stamp
//! the receive time, read to EOF, hand the bytes to a channel) and all parsing and disk IO
//! happens on a separate writer thread.
//!
//! The payload carries no timestamp, so the receive time is stamped here on both clocks. komorebi
//! sends the notification *after* its handler has run, so the receive time marks the **end** of
//! handling the event, not its start: the `processing event` line in komorebi's log is when the
//! handler began (and, at DEBUG, the `pushed notification to subscriber` line is when it sent
//! this). `komorebi-recorder trace` ties the two together.

use crate::clock;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use crossbeam_channel::Receiver;
use crossbeam_channel::Sender;
use komorebi_client::SocketMessage;
use komorebi_client::SubscribeOptions;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use std::fs::File;
use std::io::BufWriter;
use std::io::Read;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::thread::JoinHandle;
use std::time::Duration;

/// How long to keep accepting after asking komorebi to forget us.
const UNSUBSCRIBE_DRAIN: Duration = Duration::from_millis(250);

fn subscribe_to_komorebi(name: &str) -> std::io::Result<()> {
    // unfiltered: we want every event, not just the ones that changed state
    komorebi_client::send_message(&SocketMessage::AddSubscriberSocketWithOptions(
        name.to_string(),
        SubscribeOptions {
            filter_state_changes: false,
        },
    ))
}

/// A notification as it arrived, before any parsing.
struct RawNotification {
    seq: u64,
    recv_wall_us: i64,
    recv_mach_ns: u64,
    bytes: Vec<u8>,
}

/// One line of `events.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRow {
    pub seq: u64,
    pub recv_wall_us: i64,
    pub recv_mach_ns: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<Value>,
    /// The id of the stored state snapshot (`states/<id>.json`) in effect after this event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parse_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

/// Something the app-discovery logic wants to know about as it happens.
pub enum Discovery {
    /// The state changed.
    State(Arc<Value>),
    /// A window manager event mentioned this process.
    Pid(i32),
}

#[derive(Debug, Default)]
pub struct EventCounters {
    pub events: AtomicU64,
    pub states: AtomicU64,
    pub bytes: AtomicU64,
    pub parse_errors: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EventSummary {
    pub events: u64,
    pub state_snapshots: u64,
    pub bytes: u64,
    pub parse_errors: u64,
}

/// `states/<id>.json` for a state id.
#[must_use]
pub fn state_file_name(id: u64) -> String {
    format!("states/{id:06}.json")
}

pub struct EventRecorder {
    name: String,
    socket: PathBuf,
    subscribed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    counters: Arc<EventCounters>,
    accept: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
}

impl EventRecorder {
    /// Listen for komorebi's notifications, writing `events.jsonl` and `states/` into `dir`, and
    /// subscribe to komorebi if it is running.
    ///
    /// komorebi not running is not an error: the listener is bound regardless, so that when it
    /// starts (or restarts, which forgets every subscriber) a later [`subscribe`](Self::subscribe)
    /// picks the notifications up without losing the listener or the events file.
    pub fn start(dir: &Path, discovery: Option<Sender<Discovery>>) -> eyre::Result<Self> {
        std::fs::create_dir_all(dir.join("states"))?;

        let name = format!("komorebi-recorder-{}", std::process::id());
        let socket = komorebi_client::DATA_DIR.join(&name);

        match std::fs::remove_file(&socket) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).wrap_err("could not clear a stale subscriber socket"),
        }

        let listener = std::os::unix::net::UnixListener::bind(&socket)
            .wrap_err_with(|| format!("could not listen on {}", socket.display()))?;

        let subscribed = Arc::new(AtomicBool::new(subscribe_to_komorebi(&name).is_ok()));

        let events_file = File::create(dir.join("events.jsonl"))?;
        let (tx, rx) = crossbeam_channel::unbounded::<RawNotification>();
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(EventCounters::default());

        let accept_stop = stop.clone();
        let accept = std::thread::Builder::new()
            .name("komorebi-events-accept".to_string())
            .spawn(move || accept_loop(&listener, &tx, &accept_stop))?;

        let writer_counters = counters.clone();
        let states_dir = dir.join("states");
        let writer = std::thread::Builder::new()
            .name("komorebi-events-write".to_string())
            .spawn(move || {
                write_loop(
                    &rx,
                    events_file,
                    &states_dir,
                    &writer_counters,
                    discovery.as_ref(),
                )
            })?;

        Ok(Self {
            name,
            socket,
            subscribed,
            stop,
            counters,
            accept: Some(accept),
            writer: Some(writer),
        })
    }

    #[must_use]
    pub fn counters(&self) -> Arc<EventCounters> {
        self.counters.clone()
    }

    /// The name this recorder is registered under with komorebi.
    #[must_use]
    pub fn subscriber_name(&self) -> &str {
        &self.name
    }

    /// Whether komorebi currently knows about this subscriber.
    #[must_use]
    pub fn is_subscribed(&self) -> bool {
        self.subscribed.load(Ordering::Relaxed)
    }

    /// (Re-)register with komorebi. Safe to call at any time; komorebi forgets its subscribers
    /// when it restarts.
    pub fn subscribe(&self) -> std::io::Result<()> {
        let result = subscribe_to_komorebi(&self.name);
        self.subscribed.store(result.is_ok(), Ordering::Relaxed);
        result
    }

    /// Unsubscribe, drain everything already received, and report totals.
    pub fn stop(mut self) -> EventSummary {
        self.shutdown();
        EventSummary {
            events: self.counters.events.load(Ordering::Relaxed),
            state_snapshots: self.counters.states.load(Ordering::Relaxed),
            bytes: self.counters.bytes.load(Ordering::Relaxed),
            parse_errors: self.counters.parse_errors.load(Ordering::Relaxed),
        }
    }

    fn shutdown(&mut self) {
        // tell komorebi first so it stops connecting while we wind down
        let _ = komorebi_client::send_message(&SocketMessage::RemoveSubscriberSocket(
            self.name.clone(),
        ));

        // Keep reading for a moment. If komorebi was mid-notification when we unsubscribed, a
        // reader that has already gone away would make its write fail, which aborts delivery of
        // that event to its *other* subscribers (e.g. the bar). Draining lets it finish cleanly.
        std::thread::sleep(UNSUBSCRIBE_DRAIN);

        self.stop.store(true, Ordering::SeqCst);
        // wake the blocking accept so it observes the stop flag
        let _ = UnixStream::connect(&self.socket);

        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }

        // the accept thread owned the only sender, so the writer drains and ends
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }

        let _ = std::fs::remove_file(&self.socket);
    }
}

impl Drop for EventRecorder {
    fn drop(&mut self) {
        if self.accept.is_some() || self.writer.is_some() {
            self.shutdown();
        }
    }
}

fn accept_loop(
    listener: &std::os::unix::net::UnixListener,
    tx: &Sender<RawNotification>,
    stop: &AtomicBool,
) {
    let mut seq = 0_u64;

    loop {
        let (mut stream, _) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(_) if stop.load(Ordering::SeqCst) => return,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };

        // stamp before anything else: this is the number that matters
        let recv_mach_ns = clock::mach_now_ns();
        let recv_wall_us = clock::wall_now_us();

        if stop.load(Ordering::SeqCst) {
            return;
        }

        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut bytes = Vec::with_capacity(64 * 1024);
        let _ = stream.read_to_end(&mut bytes);
        if bytes.is_empty() {
            continue;
        }

        seq += 1;
        if tx
            .send(RawNotification {
                seq,
                recv_wall_us,
                recv_mach_ns,
                bytes,
            })
            .is_err()
        {
            return;
        }
    }
}

fn write_loop(
    rx: &Receiver<RawNotification>,
    events_file: File,
    states_dir: &Path,
    counters: &EventCounters,
    discovery: Option<&Sender<Discovery>>,
) {
    let mut events = BufWriter::new(events_file);
    let mut previous_state: Option<Value> = None;
    let mut state_id = 0_u64;

    while let Ok(raw) = rx.recv() {
        counters.events.fetch_add(1, Ordering::Relaxed);
        counters
            .bytes
            .fetch_add(raw.bytes.len() as u64, Ordering::Relaxed);

        let row = match serde_json::from_slice::<Value>(&raw.bytes) {
            Ok(mut value) => {
                let event = value.get_mut("event").map(Value::take);
                let state = value.get_mut("state").map(Value::take);

                if let Some(pid) = event
                    .as_ref()
                    .and_then(crate::sources::managed_apps::event_pid)
                    && let Some(discovery) = discovery
                {
                    let _ = discovery.send(Discovery::Pid(pid));
                }

                if let Some(state) = state
                    && previous_state.as_ref() != Some(&state)
                {
                    state_id += 1;
                    write_state(states_dir, state_id, &state);
                    counters.states.fetch_add(1, Ordering::Relaxed);

                    if let Some(discovery) = discovery {
                        let _ = discovery.send(Discovery::State(Arc::new(state.clone())));
                    }

                    previous_state = Some(state);
                }

                EventRow {
                    seq: raw.seq,
                    recv_wall_us: raw.recv_wall_us,
                    recv_mach_ns: raw.recv_mach_ns,
                    event,
                    state_id: (state_id > 0).then_some(state_id),
                    parse_error: None,
                    raw: None,
                }
            }
            Err(error) => {
                counters.parse_errors.fetch_add(1, Ordering::Relaxed);
                EventRow {
                    seq: raw.seq,
                    recv_wall_us: raw.recv_wall_us,
                    recv_mach_ns: raw.recv_mach_ns,
                    event: None,
                    state_id: (state_id > 0).then_some(state_id),
                    parse_error: Some(error.to_string()),
                    raw: Some(String::from_utf8_lossy(&raw.bytes).into_owned()),
                }
            }
        };

        if let Ok(line) = serde_json::to_string(&row) {
            let _ = writeln!(events, "{line}");
        }

        // events are sparse; keep the file current so a crash loses nothing
        if rx.is_empty() {
            let _ = events.flush();
        }
    }

    let _ = events.flush();
}

fn write_state(states_dir: &Path, id: u64, state: &Value) {
    let path = states_dir.join(format!("{id:06}.json"));
    if let Ok(text) = serde_json::to_string(state) {
        let _ = std::fs::write(path, text);
    }
}

/// Ask komorebi for its current `State` and write it to `path`, returning the parsed value.
pub fn snapshot_state(path: &Path) -> eyre::Result<Value> {
    let response = komorebi_client::send_query(&SocketMessage::State)
        .wrap_err("could not query komorebi for its state")?;
    let value: Value =
        serde_json::from_str(&response).wrap_err("komorebi returned invalid state JSON")?;
    std::fs::write(path, &response)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_ids_map_to_ordered_file_names() {
        assert_eq!(state_file_name(1), "states/000001.json");
        assert_eq!(state_file_name(123_456), "states/123456.json");
    }

    #[test]
    fn event_rows_roundtrip_and_skip_empty_fields() {
        let row = EventRow {
            seq: 3,
            recv_wall_us: 10,
            recv_mach_ns: 20,
            event: Some(serde_json::json!({"type": "Show"})),
            state_id: Some(2),
            parse_error: None,
            raw: None,
        };
        let text = serde_json::to_string(&row).expect("serialize");
        assert!(!text.contains("parse_error"));
        let back: EventRow = serde_json::from_str(&text).expect("parse");
        assert_eq!(back.seq, 3);
        assert_eq!(back.state_id, Some(2));
    }

    #[test]
    fn the_writer_dedupes_unchanged_states_and_tolerates_garbage() {
        let dir =
            std::env::temp_dir().join(format!("komorebi-recorder-events-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("states")).expect("dir");

        let (tx, rx) = crossbeam_channel::unbounded();
        let notification = |seq: u64, state: &str| RawNotification {
            seq,
            recv_wall_us: seq as i64,
            recv_mach_ns: seq * 1000,
            bytes: format!(r#"{{"event":{{"type":"Show","content":[null,1]}},"state":{state}}}"#)
                .into_bytes(),
        };

        tx.send(notification(1, r#"{"a":1}"#)).expect("send");
        tx.send(notification(2, r#"{"a":1}"#)).expect("send");
        tx.send(notification(3, r#"{"a":2}"#)).expect("send");
        tx.send(RawNotification {
            seq: 4,
            recv_wall_us: 4,
            recv_mach_ns: 4000,
            bytes: b"not json".to_vec(),
        })
        .expect("send");
        drop(tx);

        let counters = EventCounters::default();
        write_loop(
            &rx,
            File::create(dir.join("events.jsonl")).expect("file"),
            &dir.join("states"),
            &counters,
            None,
        );

        assert_eq!(counters.events.load(Ordering::Relaxed), 4);
        assert_eq!(
            counters.states.load(Ordering::Relaxed),
            2,
            "an unchanged state is not stored twice"
        );
        assert_eq!(counters.parse_errors.load(Ordering::Relaxed), 1);

        let rows: Vec<EventRow> = std::fs::read_to_string(dir.join("events.jsonl"))
            .expect("events")
            .lines()
            .map(|line| serde_json::from_str(line).expect("row"))
            .collect();
        assert_eq!(
            rows.iter().map(|row| row.state_id).collect::<Vec<_>>(),
            vec![Some(1), Some(1), Some(2), Some(2)]
        );
        assert!(rows[3].parse_error.is_some());
        assert_eq!(rows[3].raw.as_deref(), Some("not json"));

        assert!(dir.join("states/000001.json").exists());
        assert!(dir.join("states/000002.json").exists());
        assert!(!dir.join("states/000003.json").exists());
    }
}
