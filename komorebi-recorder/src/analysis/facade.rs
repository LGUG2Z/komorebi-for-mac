//! A session opened for investigation: everything the commands and the viewer ask, computed once.
//!
//! Opening is cheap; each part (events, the parsed log, the pairing of the two, states) is read the
//! first time it is needed and kept.

use super::blocks;
use super::blocks::Block;
use super::blocks::Pair;
use super::classify;
use super::classify::Classification;
use super::envelope;
use super::events;
use super::events::EventRecord;
use super::guard::SessionPath;
use super::logparse;
use super::logparse::Line;
use super::state::Snapshot;
use super::timespec::Edge;
use super::timespec::Lookup;
use super::timespec::Resolved;
use super::timespec::TimeSpec;
use crate::annotations;
use crate::clock::ClockMap;
use crate::incidents::INCIDENTS_FILE;
use crate::incidents::Incident;
use crate::session::Manifest;
use crate::session::STATUS_RECORDING;
use crate::session::Session;
use crate::sources::komorebi_events::state_file_name;
use crate::timeline;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;

/// The parsed komorebi log of a session.
pub struct LogData {
    pub lines: Vec<Line>,
    pub blocks: Vec<Block>,
}

/// Which handler produced which event, and what kind of event it was.
pub struct Pairing {
    /// The name the recorder subscribed as to komorebi.
    pub subscriber: Option<String>,
    /// One per event, in event order.
    pub pairs: Vec<Pair>,
    pub classes: Vec<Classification>,
}

/// Where the state at a moment came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateSource {
    /// The notification of this event.
    Event { seq: u64, recv_mach_ns: u64 },
    /// The state recorded when the recording started, before any event.
    Start,
}

pub struct StateAt {
    pub snapshot: Arc<Snapshot>,
    pub state_id: Option<u64>,
    pub source: StateSource,
}

pub struct Analysis {
    pub session: Session,
    pub manifest: Manifest,
    guard: SessionPath,
    clock: ClockMap,
    events: OnceLock<(Vec<EventRecord>, Vec<String>)>,
    log: OnceLock<LogData>,
    pairing: OnceLock<Pairing>,
    start_state: OnceLock<Option<Arc<Snapshot>>>,
    states: Mutex<HashMap<u64, Arc<Snapshot>>>,
}

impl Analysis {
    /// Open a finished session. One that is still being recorded, or whose recorder died, cannot
    /// be analyzed yet and says how to proceed.
    pub fn open(dir: &Path) -> eyre::Result<Self> {
        let guard = SessionPath::new(dir)?;
        let session = Session::open(guard.root())?;
        let manifest = session.manifest()?;

        if manifest.status == STATUS_RECORDING {
            let shown = session.dir.display();
            return Err(match crate::lock::recorder(&session.dir) {
                crate::lock::Recorder::Running(_) => envelope::unusable(format!(
                    "{shown} is still being recorded; stop the recording first"
                )),
                _ => envelope::with_hint(
                    envelope::unusable(format!(
                        "{shown} was left unfinished: its recorder was killed or crashed"
                    )),
                    format!("recover it with `komorebi-recorder reindex {shown}`"),
                ),
            });
        }

        let clock = ClockMap::new(manifest.anchors.clone());
        Ok(Self {
            session,
            manifest,
            guard,
            clock,
            events: OnceLock::new(),
            log: OnceLock::new(),
            pairing: OnceLock::new(),
            start_state: OnceLock::new(),
            states: Mutex::new(HashMap::new()),
        })
    }

    #[must_use]
    pub fn guard(&self) -> &SessionPath {
        &self.guard
    }

    #[must_use]
    pub fn clock(&self) -> &ClockMap {
        &self.clock
    }

    fn loaded_events(&self) -> &(Vec<EventRecord>, Vec<String>) {
        self.events.get_or_init(|| {
            events::load(&self.session).unwrap_or_else(|error| {
                (
                    Vec::new(),
                    vec![format!("could not read the events: {error:#}")],
                )
            })
        })
    }

    /// Every komorebi notification, in the order received.
    #[must_use]
    pub fn events(&self) -> &[EventRecord] {
        &self.loaded_events().0
    }

    /// Things that went wrong reading the session's files; shown with any answer.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        self.loaded_events().1.clone()
    }

    #[must_use]
    pub fn event_index(&self, seq: u64) -> Option<usize> {
        self.events().iter().position(|event| event.seq == seq)
    }

    /// komorebi's log, parsed, with its handler blocks.
    #[must_use]
    pub fn log(&self) -> &LogData {
        self.log.get_or_init(|| {
            let bytes = std::fs::read(self.session.path("komorebi.log")).unwrap_or_default();
            let lines = logparse::parse(&bytes);
            let blocks = blocks::build(&lines, &self.clock);
            LogData { lines, blocks }
        })
    }

    #[must_use]
    pub fn pairing(&self) -> &Pairing {
        self.pairing.get_or_init(|| {
            let log = self.log();
            let events = self.events();
            // the recorder wrote its own name into the manifest; older sessions are worked out from
            // which of komorebi's "pushed notification" lines line up with the events
            let subscriber = self
                .manifest
                .komorebi
                .subscriber
                .clone()
                .or_else(|| blocks::infer_subscriber(&log.blocks, events));
            Pairing {
                pairs: blocks::pair(events, &log.blocks, subscriber.as_deref()),
                classes: classify::classify(events),
                subscriber,
            }
        })
    }

    /// Whether the state changed at event `index`: `None` when either state is unknown.
    #[must_use]
    pub fn state_changed(&self, index: usize) -> Option<bool> {
        let events = self.events();
        let (this, before) = (
            events.get(index)?.state_id?,
            events.get(index.checked_sub(1)?)?.state_id?,
        );
        Some(this != before)
    }

    /// The state with this id (`states/<id>.json`).
    pub fn snapshot(&self, state_id: u64) -> eyre::Result<Arc<Snapshot>> {
        if let Some(found) = self
            .states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&state_id)
        {
            return Ok(found.clone());
        }

        let path = self.guard.resolve(&state_file_name(state_id))?;
        let text = std::fs::read_to_string(&path)
            .map_err(|_| envelope::not_found(format!("there is no recorded state {state_id}")))?;
        let value: serde_json::Value = serde_json::from_str(&text)
            .wrap_err_with(|| format!("{} is not valid JSON", path.display()))?;
        let snapshot = Arc::new(Snapshot::from_value(&value));
        self.states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(state_id, snapshot.clone());
        Ok(snapshot)
    }

    fn start_snapshot(&self) -> Option<Arc<Snapshot>> {
        self.start_state
            .get_or_init(|| {
                let text = std::fs::read_to_string(self.session.path("state-start.json")).ok()?;
                let value: serde_json::Value = serde_json::from_str(&text).ok()?;
                Some(Arc::new(Snapshot::from_value(&value)))
            })
            .clone()
    }

    /// The state komorebi was in at `mach_ns`: the one in the latest notification before it, or
    /// the starting state if there has been none. It is the state as of that notification, so it
    /// can be out of date if komorebi changed state without sending one since.
    pub fn state_at(&self, mach_ns: u64) -> eyre::Result<StateAt> {
        let events = self.events();
        let latest = events
            .iter()
            .rev()
            .find(|event| event.recv_mach_ns <= mach_ns && event.state_id.is_some());

        if let Some(event) = latest {
            let state_id = event.state_id.unwrap_or_default();
            return Ok(StateAt {
                snapshot: self.snapshot(state_id)?,
                state_id: Some(state_id),
                source: StateSource::Event {
                    seq: event.seq,
                    recv_mach_ns: event.recv_mach_ns,
                },
            });
        }

        self.start_snapshot()
            .map(|snapshot| StateAt {
                snapshot,
                state_id: None,
                source: StateSource::Start,
            })
            .ok_or_else(|| {
                envelope::not_found(
                    "no state was recorded before this moment (komorebi was not running, or \
                     had sent no notification yet)",
                )
            })
    }

    /// Resolve any time argument.
    pub fn resolve(&self, text: &str) -> eyre::Result<Resolved> {
        text.parse::<TimeSpec>()?.resolve(&self.manifest, self)
    }

    #[must_use]
    pub fn video_s(&self, mach_ns: u64) -> f64 {
        self.manifest.video_seconds(mach_ns)
    }

    #[must_use]
    pub fn wall_utc(&self, mach_ns: u64) -> Option<String> {
        self.clock
            .mach_to_wall(mach_ns)
            .and_then(timeline::format_wall_us)
    }
}

impl Lookup for Analysis {
    fn event_mach_ns(&self, seq: u64, edge: Edge) -> eyre::Result<u64> {
        let index = self
            .event_index(seq)
            .ok_or_else(|| envelope::not_found(format!("there is no event #{seq}")))?;
        let event = &self.events()[index];

        if edge == Edge::End {
            return Ok(event.recv_mach_ns);
        }

        // The start of an event is when its handler began, where the log says. It is clamped to
        // after the previous notification and before this one, so that the state at the start is
        // exactly the state the previous event left and never already includes this event.
        let began = self
            .pairing()
            .pairs
            .get(index)
            .and_then(|pair| pair.block)
            .and_then(|id| self.log().blocks.get(id))
            .map_or(event.recv_mach_ns, |block| block.header_mach_ns);
        let previous = index
            .checked_sub(1)
            .map_or(0, |before| self.events()[before].recv_mach_ns);
        Ok(began
            .min(event.recv_mach_ns.saturating_sub(1))
            .max(previous))
    }

    fn log_mach_ns(&self, line: u64) -> eyre::Result<u64> {
        let log = self.log();
        let parsed = usize::try_from(line.saturating_sub(1))
            .ok()
            .filter(|_| line >= 1)
            .and_then(|index| log.lines.get(index))
            .ok_or_else(|| {
                envelope::not_found(format!(
                    "komorebi.log has {} lines; there is no line {line}",
                    log.lines.len()
                ))
            })?;

        parsed
            .wall_us
            .and_then(|wall_us| self.clock.wall_to_mach(wall_us))
            .ok_or_else(|| envelope::not_found(format!("log line {line} has no time")))
    }

    fn incident_mach_ns(&self, number: u64) -> eyre::Result<u64> {
        let text = std::fs::read_to_string(self.session.path(INCIDENTS_FILE))
            .map_err(|_| envelope::not_found("the session recorded no incidents"))?;
        usize::try_from(number.saturating_sub(1))
            .ok()
            .filter(|_| number >= 1)
            .and_then(|index| text.lines().nth(index))
            .and_then(|line| serde_json::from_str::<Incident>(line).ok())
            .map(|incident| incident.mach_ns)
            .ok_or_else(|| envelope::not_found(format!("there is no incident {number}")))
    }

    fn annotation_span(&self, id: &str) -> eyre::Result<(u64, Option<u64>)> {
        let loaded = annotations::load(&self.session.dir)?;
        loaded
            .get(id)
            .map(|annotation| (annotation.start_mach_ns, annotation.end_mach_ns))
            .ok_or_else(|| envelope::not_found(format!("there is no annotation {id}")))
    }
}
