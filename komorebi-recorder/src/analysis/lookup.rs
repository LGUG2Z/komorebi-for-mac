//! [`Lookup`] over the files of a real session.

use super::envelope;
use super::timespec::Edge;
use super::timespec::Lookup;
use crate::annotations;
use crate::incidents::INCIDENTS_FILE;
use crate::incidents::Incident;
use crate::session::Manifest;
use crate::session::Session;
use crate::timeline;
use crate::timeline::Locator;
use color_eyre::eyre;

pub struct SessionLookup<'a> {
    pub session: &'a Session,
    pub manifest: &'a Manifest,
}

impl<'a> SessionLookup<'a> {
    #[must_use]
    pub fn new(session: &'a Session, manifest: &'a Manifest) -> Self {
        Self { session, manifest }
    }

    fn located(&self, locator: &Locator) -> eyre::Result<u64> {
        timeline::locate(self.session, self.manifest, locator)
            .map(|located| located.mach_ns)
            .map_err(|error| envelope::not_found(format!("{error:#}")))
    }
}

impl Lookup for SessionLookup<'_> {
    fn event_mach_ns(&self, seq: u64, _edge: Edge) -> eyre::Result<u64> {
        self.located(&Locator::EventSeq(seq))
    }

    fn log_mach_ns(&self, line: u64) -> eyre::Result<u64> {
        self.located(&Locator::LogLine(line))
    }

    fn incident_mach_ns(&self, number: u64) -> eyre::Result<u64> {
        let text = std::fs::read_to_string(self.session.path(INCIDENTS_FILE))
            .map_err(|_| envelope::not_found("the session recorded no incidents"))?;
        let index = usize::try_from(number.saturating_sub(1)).unwrap_or(usize::MAX);
        text.lines()
            .nth(index)
            .filter(|_| number >= 1)
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
