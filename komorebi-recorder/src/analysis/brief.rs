//! The first thing an investigator reads: what the user says is wrong, and what the recording shows
//! around it.
//!
//! The user's bug region and description are the ground truth of *what* went wrong and come first,
//! verbatim. Everything after them is evidence of *why*, drawn from the recording: the events in the
//! region (with komorebi's own echoes counted but not listed), what looked wrong and for how long,
//! the state as the bug began and as it ended and the difference, what happened to the machine
//! nearby, and the few events most worth tracing first, each with the reason it was chosen.

use super::Analysis;
use super::answers::ChangeView;
use super::answers::EventQuery;
use super::catalog;
use super::catalog::Skew;
use super::classify::Class;
use super::diff;
use super::invariants;
use super::invariants::Violation;
use super::overview::Finding;
use super::overview::IncidentView;
use super::overview::PRE_ROLL_NS;
use super::overview::SessionInfo;
use super::state::Focus;
use super::views::EventView;
use super::views::Moment;
use super::views::StateOrigin;
use crate::annotations::Annotation;
use crate::annotations::AnnotationView;
use crate::annotations::Kind;
use color_eyre::eyre;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;

pub const SCHEMA_BRIEF: &str = "brief/1";

/// How many non-echo events a brief lists for a region.
const LISTED_EVENTS: usize = 40;
const LISTED_CHANGES: usize = 40;
const LISTED_FINDINGS: usize = 10;
const LISTED_SUSPECTS: usize = 5;

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct StateBrief {
    pub at: Moment,
    pub origin: Option<StateOrigin>,
    pub summary: String,
    pub focus: Option<Focus>,
    pub violations: Vec<Violation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Suspect {
    pub event: EventView,
    pub score: i64,
    /// Why this event is worth tracing.
    pub reasons: Vec<String>,
    pub next: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct RegionEvents {
    pub total: usize,
    /// How many events there were of each class.
    pub by_class: BTreeMap<String, usize>,
    /// Echoes of komorebi's own moves: counted above, not listed.
    pub echoes_hidden: usize,
    pub listed: Vec<EventView>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct RegionBrief {
    /// The region padded by a few seconds before it, so causes just before it are included.
    pub from: Moment,
    pub to: Moment,
    pub events: RegionEvents,
    /// What looked wrong or unusual in or just before the region, most relevant first.
    pub findings: Vec<Finding>,
    pub suspects: Vec<Suspect>,
    pub state_at_start: Option<StateBrief>,
    pub state_at_end: Option<StateBrief>,
    /// What differs between the state at the start and at the end.
    pub changes: Vec<ChangeView>,
    pub changes_total: usize,
    pub incidents: Vec<IncidentView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct BugBrief {
    /// What the user said: the region, the description and what they expected.
    pub bug: AnnotationView,
    pub region: RegionBrief,
    /// Commands that dig further.
    pub commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct BriefData {
    pub session: SessionInfo,
    /// Whether komorebi's source is the version that was recorded.
    pub skew: Skew,
    pub bugs: Vec<BugBrief>,
    /// With no bug marked: the same digest over the whole recording.
    pub whole_session: Option<RegionBrief>,
    /// Conclusions an earlier investigation recorded.
    pub findings_recorded: Vec<AnnotationView>,
    pub notes: Vec<AnnotationView>,
    pub next: Vec<String>,
}

impl Analysis {
    fn state_brief(&self, mach_ns: u64) -> Option<StateBrief> {
        let found = self.state_at(mach_ns).ok()?;
        let data = self
            .state_answer(mach_ns, &super::answers::StateQuery::default())
            .ok()?;
        Some(StateBrief {
            at: data.at,
            origin: Some(data.origin),
            summary: super::views::headline(&found.snapshot),
            focus: data.focus,
            violations: data.violations,
        })
    }

    /// Score each event in `from..=to` by how likely it is to be where things went wrong.
    fn suspects(&self, from_ns: u64, to_ns: u64, bug_start_ns: u64) -> Vec<Suspect> {
        let events = self.events();
        let pairing = self.pairing();
        let mut ranked = Vec::new();

        for (index, event) in events.iter().enumerate() {
            if event.recv_mach_ns < from_ns || event.recv_mach_ns > to_ns {
                continue;
            }

            let class = pairing.classes[index].class;
            if matches!(class, Class::Echo | Class::Manual) {
                continue;
            }

            let mut score = 0_i64;
            let mut reasons = Vec::new();

            score += match class {
                Class::Command | Class::Monitor | Class::Lock => 40,
                Class::Lifecycle => 20,
                Class::Focus => 10,
                _ => 5,
            };

            // did it leave the state wrong?
            if let (Some(this), Some(before)) = (
                event.state_id,
                index
                    .checked_sub(1)
                    .and_then(|before| events[before].state_id),
            ) && this != before
                && let (Ok(after), Ok(previous)) = (self.snapshot(this), self.snapshot(before))
            {
                let (now, was) = (invariants::check(&after), invariants::check(&previous));
                let appeared: Vec<&Violation> = now.iter().filter(|v| !was.contains(v)).collect();
                if !appeared.is_empty() {
                    score += 100;
                    reasons.push(format!(
                        "left the state wrong: {}",
                        appeared
                            .iter()
                            .map(|violation| violation.kind.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if diff::diff(&previous, &after).is_empty() {
                    // the state was rewritten but nothing a person would notice changed
                } else {
                    score += 15;
                    reasons.push("changed the state".to_string());
                }
            }

            if let Some(handler) = &pairing
                .pairs
                .get(index)
                .and_then(|pair| pair.block)
                .and_then(|id| self.log().blocks.get(id))
            {
                if handler.errors > 0 {
                    score += 50;
                    reasons.push(format!("its handler logged {} error(s)", handler.errors));
                } else if handler.warnings > 0 {
                    score += 25;
                    reasons.push(format!(
                        "its handler logged {} warning(s)",
                        handler.warnings
                    ));
                }
            }
            if pairing.pairs[index]
                .handler_ms
                .is_some_and(|ms| ms >= 250.0)
            {
                score += 30;
                reasons.push("its handler was slow".to_string());
            }

            // what happens as the bug begins matters most
            let near_start = event.recv_mach_ns.abs_diff(bug_start_ns) <= 1_000_000_000;
            if near_start {
                score += 20;
                reasons.push("right at the start of the region".to_string());
            }

            if reasons.is_empty() {
                continue;
            }

            if let Some(view) = self.event_view(index) {
                ranked.push(Suspect {
                    next: format!(
                        "komorebi-recorder trace {} event:{}",
                        self.session.dir.display(),
                        event.seq
                    ),
                    event: view,
                    score,
                    reasons,
                });
            }
        }

        ranked.sort_by(|a, b| b.score.cmp(&a.score).then(a.event.seq.cmp(&b.event.seq)));
        ranked.truncate(LISTED_SUSPECTS);
        ranked
    }

    fn region_brief(
        &self,
        from_ns: u64,
        to_ns: u64,
        bug_start_ns: u64,
        findings: &[Finding],
    ) -> eyre::Result<RegionBrief> {
        let answer = self.events_answer(&EventQuery {
            from_mach_ns: Some(from_ns),
            to_mach_ns: Some(to_ns),
            ..EventQuery::default()
        });
        let mut by_class: BTreeMap<String, usize> = BTreeMap::new();
        for view in &answer.data.events {
            *by_class
                .entry(format!("{:?}", view.class).to_lowercase())
                .or_default() += 1;
        }
        let echoes = answer
            .data
            .events
            .iter()
            .filter(|view| view.class == Class::Echo)
            .count();
        let real: Vec<EventView> = answer
            .data
            .events
            .into_iter()
            .filter(|view| view.class != Class::Echo)
            .collect();
        let truncated = real.len() > LISTED_EVENTS;

        let start = self.state_brief(from_ns.max(bug_start_ns.min(to_ns)));
        let end = self.state_brief(to_ns);
        let changes = match (self.state_at(bug_start_ns.min(to_ns)), self.state_at(to_ns)) {
            (Ok(before), Ok(after)) => diff::diff(&before.snapshot, &after.snapshot),
            _ => Vec::new(),
        };
        let changes_total = changes.len();

        let incidents: Vec<IncidentView> = self
            .incidents()
            .into_iter()
            .filter(|incident| {
                let at = self.manifest.mach_at_video_seconds(incident.video_s);
                at + PRE_ROLL_NS >= from_ns && at <= to_ns + PRE_ROLL_NS
            })
            .collect();

        Ok(RegionBrief {
            from: self.moment(from_ns),
            to: self.moment(to_ns),
            events: RegionEvents {
                total: real.len() + echoes,
                by_class,
                echoes_hidden: echoes,
                listed: real.into_iter().take(LISTED_EVENTS).collect(),
                truncated,
            },
            findings: findings
                .iter()
                .filter(|finding| finding.end_mach_ns >= from_ns && finding.start_mach_ns <= to_ns)
                .take(LISTED_FINDINGS)
                .cloned()
                .collect(),
            suspects: self.suspects(from_ns, to_ns, bug_start_ns),
            state_at_start: start,
            state_at_end: end,
            changes: changes
                .into_iter()
                .take(LISTED_CHANGES)
                .map(|change| ChangeView {
                    text: change.describe(),
                    change,
                })
                .collect(),
            changes_total,
            incidents,
        })
    }

    /// The brief of the session: one entry per bug region the user marked, or, if none, a digest
    /// of the whole recording.
    pub fn brief(&self) -> eyre::Result<BriefData> {
        let loaded = crate::annotations::load(&self.session.dir)?;
        let bugs: Vec<&Annotation> = loaded.bugs().collect();
        let all = self.findings()?;
        let dir = self.session.dir.display().to_string();

        let mut briefs = Vec::new();
        for bug in &bugs {
            let end = bug.end_or_session_end(&self.manifest);
            let from = bug.start_mach_ns.saturating_sub(PRE_ROLL_NS / 2);
            let ranked = self.rank(all.clone(), &[bug]);
            let region = self.region_brief(from, end, bug.start_mach_ns, &ranked)?;

            let mut commands = vec![
                format!(
                    "komorebi-recorder events {dir} --from bug:{id} --to bug:{id}.end --hide-echo",
                    id = bug.id
                ),
                format!(
                    "komorebi-recorder diff {dir} bug:{id} bug:{id}.end",
                    id = bug.id
                ),
            ];
            if let Some(suspect) = region.suspects.first() {
                commands.insert(0, suspect.next.clone());
                commands.push(format!(
                    "komorebi-recorder code {dir} event:{}",
                    suspect.event.seq
                ));
            }
            if !self.manifest.displays.is_empty() {
                commands.push(format!("komorebi-recorder sheet {dir} --bug {}", bug.id));
                commands.push(format!("komorebi-recorder changes {dir} --bug {}", bug.id));
            }
            for window in region
                .findings
                .iter()
                .flat_map(|finding| finding.window_ids.iter())
                .take(1)
            {
                commands.push(format!("komorebi-recorder window {dir} {window}"));
            }
            commands.push(format!(
                "komorebi-recorder annotate {dir} add --kind finding --about {} --text '<cause>' --ref event:<n> --ref code:<path>:<line>",
                bug.id
            ));

            briefs.push(BugBrief {
                bug: bug.view(&self.manifest),
                region,
                commands,
            });
        }

        let whole_session = if bugs.is_empty() {
            let end = crate::analysis::timespec::session_end_ns(&self.manifest);
            let start = self.manifest.video_t0();
            let ranked = self.rank(all, &[]);
            Some(self.region_brief(start, end, start, &ranked)?)
        } else {
            None
        };

        let mut next = Vec::new();
        if bugs.is_empty() {
            next.push(format!(
                "no bug region was marked: ask the user when it happened and what it looked like, \
                 then `komorebi-recorder annotate {dir} add --kind bug --start <t> --end <t> --text '<what is wrong>'`"
            ));
        }
        next.push(format!("komorebi-recorder overview {dir}"));
        next.push("komorebi-recorder guide".to_string());

        let recorded: Vec<AnnotationView> = loaded
            .annotations
            .iter()
            .filter(|annotation| annotation.kind == Kind::Finding)
            .map(|annotation| annotation.view(&self.manifest))
            .collect();
        let notes: Vec<AnnotationView> = loaded
            .annotations
            .iter()
            .filter(|annotation| annotation.kind == Kind::Note)
            .map(|annotation| annotation.view(&self.manifest))
            .collect();

        Ok(BriefData {
            session: self.session_info(),
            skew: catalog::skew(&self.manifest.komorebi.versions),
            bugs: briefs,
            whole_session,
            findings_recorded: recorded,
            notes,
            next,
        })
    }
}
