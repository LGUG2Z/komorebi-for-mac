//! The JSON Schema of every command's output, so a caller can know what to expect.

use super::answers::AnnotationData;
use super::answers::AnnotationsData;
use super::answers::DeletedData;
use super::answers::DiffData;
use super::answers::EventsData;
use super::answers::StateData;
use super::brief::BriefData;
use super::changes::ChangesData;
use super::code::CodeData;
use super::envelope::Envelope;
use super::history::HistoryData;
use super::overview::OverviewData;
use super::search::SearchData;
use super::sheet::SheetData;
use super::trace::TraceData;
use schemars::schema_for;
use serde_json::Value;

/// A command output's schema: its name, a sentence about it, and the JSON Schema of its `data`.
pub struct Entry {
    pub name: &'static str,
    pub about: &'static str,
    pub schema: fn() -> Value,
}

fn of<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schema_for!(T)).unwrap_or(Value::Null)
}

/// Every schema, by the name that appears in an envelope's `schema`.
pub const ALL: &[Entry] = &[
    Entry {
        name: "brief/1",
        about: "`brief`: the user's bug regions and what the recording shows around each",
        schema: of::<BriefData>,
    },
    Entry {
        name: "overview/1",
        about: "`overview`: ranked findings, a histogram of the recording and its incidents",
        schema: of::<OverviewData>,
    },
    Entry {
        name: "state/1",
        about: "`state`: komorebi's state at a moment, with anything wrong in it",
        schema: of::<StateData>,
    },
    Entry {
        name: "diff/1",
        about: "`diff`: what changed between two moments",
        schema: of::<DiffData>,
    },
    Entry {
        name: "events/1",
        about: "`events`: komorebi's events, classified, with the handler that ran",
        schema: of::<EventsData>,
    },
    Entry {
        name: "trace/1",
        about: "`trace`: one event end to end with attributed changes",
        schema: of::<TraceData>,
    },
    Entry {
        name: "window/1",
        about: "`window`: one window's life through the recording",
        schema: of::<HistoryData>,
    },
    Entry {
        name: "code/1",
        about: "`code`: log lines mapped to komorebi's source",
        schema: of::<CodeData>,
    },
    Entry {
        name: "search/1",
        about: "`search`: where a word appears in the recording",
        schema: of::<SearchData>,
    },
    Entry {
        name: "changes/1",
        about: "`changes`: when and where the picture changed on each display",
        schema: of::<ChangesData>,
    },
    Entry {
        name: "sheet/1",
        about: "`sheet`: a contact sheet image and where each frame is on it",
        schema: of::<SheetData>,
    },
    Entry {
        name: "annotation/1",
        about: "`annotate add|edit`: the annotation as it now reads",
        schema: of::<AnnotationData>,
    },
    Entry {
        name: "annotations/1",
        about: "`annotations`: bug regions, notes and findings",
        schema: of::<AnnotationsData>,
    },
    Entry {
        name: "annotation-deleted/1",
        about: "`annotate delete`: the id removed",
        schema: of::<DeletedData>,
    },
    Entry {
        name: "error/1",
        about: "any failed command: the envelope with `ok: false` and an `error`",
        schema: of::<Envelope<()>>,
    },
];

/// The schema named `name`.
#[must_use]
pub fn find(name: &str) -> Option<&'static Entry> {
    ALL.iter().find(|entry| entry.name == name)
}
