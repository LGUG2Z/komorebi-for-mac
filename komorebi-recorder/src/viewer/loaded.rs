//! A session opened in the viewer: everything the window draws, gathered once off the UI thread.

use crate::analysis::Analysis;
use crate::analysis::catalog::Skew;
use crate::analysis::logparse::Level;
use crate::analysis::overview::Finding;
use crate::analysis::overview::IncidentView;
use crate::analysis::views::EventView;
use crate::annotations;
use crate::annotations::AnnotationView;
use color_eyre::eyre;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

/// A display and the stretches of the recording that have video from it.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayMeta {
    pub id: u32,
    /// `[x, y, width, height]` in global points: where its picture sits among the monitors.
    pub frame: [f64; 4],
    /// Seconds of video time with video from this display.
    pub segments: Vec<(f64, f64)>,
}

/// An error or warning in komorebi's log.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogMark {
    pub video_s: f64,
    pub level: Level,
}

pub struct Loaded {
    pub analysis: Arc<Analysis>,
    pub dir: PathBuf,
    pub duration_s: f64,
    pub events: Vec<EventView>,
    /// Video time of each event, for finding the one nearest a point.
    pub event_times: Vec<f64>,
    pub incidents: Vec<IncidentView>,
    /// Ranked, most worth looking at first.
    pub findings: Vec<Finding>,
    pub log_marks: Vec<LogMark>,
    /// Video time of each line of komorebi's log (NaN where a line has no time).
    pub log_times: Vec<f64>,
    pub displays: Vec<DisplayMeta>,
    /// Bug regions, notes and findings.
    pub annotations: Vec<AnnotationView>,
    pub warnings: Vec<String>,
    pub skew: Skew,
}

impl Loaded {
    /// Open the session in `dir`. Reads everything the window needs; call it off the UI thread.
    pub fn load(dir: &Path) -> eyre::Result<Self> {
        let analysis = Arc::new(Analysis::open(dir)?);
        let duration_s = analysis.manifest.video_span().1;

        let events: Vec<EventView> = (0..analysis.events().len())
            .filter_map(|index| analysis.event_view(index))
            .collect();
        let event_times = events.iter().map(|event| event.video_s).collect();

        let bugs = analysis.bugs()?;
        let bug_refs: Vec<&annotations::Annotation> = bugs.iter().collect();
        let findings = analysis.rank(analysis.findings()?, &bug_refs);

        let log_marks = analysis
            .log()
            .lines
            .iter()
            .filter_map(|line| {
                let level = line
                    .level
                    .filter(|level| matches!(level, Level::Error | Level::Warn))?;
                let mach_ns = analysis.clock().wall_to_mach(line.wall_us?)?;
                Some(LogMark {
                    video_s: analysis.video_s(mach_ns),
                    level,
                })
            })
            .collect();

        let log_times: Vec<f64> = analysis
            .log()
            .lines
            .iter()
            .map(|line| {
                line.wall_us
                    .and_then(|wall_us| analysis.clock().wall_to_mach(wall_us))
                    .map_or(f64::NAN, |mach_ns| analysis.video_s(mach_ns))
            })
            .collect();

        let mut displays: Vec<DisplayMeta> = Vec::new();
        for entry in analysis
            .manifest
            .displays
            .iter()
            .filter(|entry| entry.result.frames > 0)
        {
            let id = entry.result.display.id;
            let end = entry.end_s.unwrap_or(duration_s);
            match displays.iter_mut().find(|meta| meta.id == id) {
                Some(meta) => meta.segments.push((entry.offset_s, end)),
                None => displays.push(DisplayMeta {
                    id,
                    frame: entry.result.display.frame,
                    segments: vec![(entry.offset_s, end)],
                }),
            }
        }
        displays.sort_by_key(|meta| meta.id);

        let loaded_annotations = annotations::load(dir)?;
        let mut warnings = analysis.warnings();
        warnings.extend(loaded_annotations.warnings.clone());
        let skew = crate::analysis::catalog::skew(&analysis.manifest.komorebi.versions);

        Ok(Self {
            incidents: analysis.incidents(),
            annotations: loaded_annotations
                .annotations
                .iter()
                .map(|annotation| annotation.view(&analysis.manifest))
                .collect(),
            analysis,
            dir: dir.to_path_buf(),
            duration_s,
            events,
            event_times,
            findings,
            log_marks,
            log_times,
            displays,
            warnings,
            skew,
        })
    }

    /// Read the annotations again, after one was added, changed or removed.
    pub fn reload_annotations(&mut self) -> eyre::Result<()> {
        let loaded = annotations::load(&self.dir)?;
        self.annotations = loaded
            .annotations
            .iter()
            .map(|annotation| annotation.view(&self.analysis.manifest))
            .collect();
        Ok(())
    }

    /// The bug regions, in time order.
    pub fn bugs(&self) -> impl Iterator<Item = &AnnotationView> {
        self.annotations
            .iter()
            .filter(|annotation| annotation.kind == annotations::Kind::Bug)
    }

    /// The event with this sequence number.
    #[must_use]
    pub fn event(&self, seq: u64) -> Option<&EventView> {
        self.events.iter().find(|event| event.seq == seq)
    }
}

/// A name for a session in a list: the folder it is in.
#[must_use]
pub fn name_of(dir: &Path) -> String {
    dir.file_name().map_or_else(
        || dir.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::SessionBuilder;
    use serde_json::json;

    #[test]
    fn a_session_loads_with_everything_the_window_draws() {
        let (session, _) = SessionBuilder::new("viewer-loaded")
            .event(
                2.0,
                json!({"type": "Retile"}),
                Some(crate::testkit::state_with_windows(&[(1, "a")])),
            )
            .log(
                2.0,
                "ERROR",
                "komorebi::macos_api: failed to get window position: AXError::InvalidUIElement",
            )
            .incident(3.0, "display-disconnected", "display 3 went away")
            .build();

        let loaded = Loaded::load(&session.dir).expect("load");
        assert_eq!(loaded.events.len(), 1);
        assert_eq!(loaded.event_times.len(), 1);
        assert!((loaded.event_times[0] - 2.0).abs() < 1e-6);
        assert_eq!(loaded.incidents.len(), 1);
        assert_eq!(loaded.log_marks.len(), 1);
        assert_eq!(loaded.log_marks[0].level, Level::Error);
        assert_eq!(loaded.log_times.len(), 1);
        assert!((loaded.log_times[0] - 2.0).abs() < 1e-6);
        assert!((loaded.duration_s - 10.0).abs() < 1e-6);
        assert!(loaded.displays.is_empty(), "no video was recorded");
        assert!(!loaded.findings.is_empty());
    }

    #[test]
    fn annotations_are_read_again_after_a_change() {
        let (session, manifest) = SessionBuilder::new("viewer-reload").build();
        let mut loaded = Loaded::load(&session.dir).expect("load");
        assert_eq!(loaded.bugs().count(), 0);

        annotations::add(
            &session.dir,
            annotations::New {
                kind: Some(annotations::Kind::Bug),
                start_mach_ns: manifest.mach_at_video_seconds(1.0),
                end_mach_ns: Some(manifest.mach_at_video_seconds(2.0)),
                text: "wrong".to_string(),
                ..annotations::New::default()
            },
        )
        .expect("add");

        loaded.reload_annotations().expect("reload");
        assert_eq!(loaded.bugs().count(), 1);
        assert!((loaded.bugs().next().expect("bug").start_video_s - 1.0).abs() < 1e-6);
    }

    #[test]
    fn a_session_that_cannot_be_analyzed_says_why() {
        let (session, mut manifest) = SessionBuilder::new("viewer-live").build();
        manifest.status = "recording".to_string();
        session.write_manifest(&manifest).expect("manifest");

        let error = Loaded::load(&session.dir)
            .err()
            .expect("refused")
            .to_string();
        assert!(
            error.contains("left unfinished") || error.contains("still being recorded"),
            "{error}"
        );
        assert!(Loaded::load(&session.dir.join("nope")).is_err());
    }

    #[test]
    fn a_session_is_named_for_its_folder() {
        assert_eq!(
            name_of(Path::new("/a/b/20261026T000000Z")),
            "20261026T000000Z"
        );
    }
}
