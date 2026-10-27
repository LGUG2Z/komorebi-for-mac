//! What stands out in a recording, ranked by how likely it is to matter.
//!
//! A session holds thousands of events and log lines, nearly all of them ordinary. This looks for
//! the things that are not: states that are wrong and for how long, windows that flap, bursts of
//! events, slow handlers, errors, and what happened to the machine (a display unplugged, a sleep).
//! Each is a [`Finding`] with the event or log line to look at next. Findings that overlap, or come
//! just before, a region the user marked as the bug rank first.

use super::Analysis;
use super::classify::Class;
use super::events::Family;
use super::invariants;
use super::invariants::Severity;
use super::logparse::Level;
use super::state::Snapshot;
use crate::annotations::Annotation;
use crate::annotations::Kind;
use crate::incidents::INCIDENTS_FILE;
use crate::incidents::Incident;
use crate::timecode;
use color_eyre::eyre;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;

pub const SCHEMA_OVERVIEW: &str = "overview/1";

/// How long before a bug region starts a finding still counts as a possible cause.
pub const PRE_ROLL_NS: u64 = 10_000_000_000;

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Finding {
    /// `invariant:<kind>`, `window-flapping`, `focus-oscillation`, `event-storm`, `slow-handler`,
    /// `log-error`, `log-warning`, `monitor-churn` or `incident:<kind>`.
    pub kind: String,
    pub severity: Severity,
    /// Higher is more worth looking at.
    pub score: i64,
    pub summary: String,
    pub start_video_s: f64,
    pub end_video_s: f64,
    pub start_mach_ns: u64,
    pub end_mach_ns: u64,
    /// What to look at: `event:12`, `log:345`, `incident:2`.
    pub refs: Vec<String>,
    pub window_ids: Vec<u64>,
    /// Still the case when the recording ended.
    pub persisted_to_end: bool,
    /// The bug regions it overlaps, or comes within the pre-roll before.
    pub bugs: Vec<String>,
    /// A command that digs into it.
    pub next: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct IncidentView {
    /// `incident:<number>`
    pub number: u64,
    pub video_s: f64,
    pub kind: String,
    pub detail: String,
    pub display: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct DisplayInfo {
    pub id: u32,
    pub width_px: usize,
    pub height_px: usize,
    pub segments: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SessionInfo {
    pub dir: String,
    pub status: String,
    pub started_utc: Option<String>,
    /// Seconds of video: the span findings and bug regions are placed in.
    pub duration_s: f64,
    pub recorder_version: String,
    pub macos_version: Option<String>,
    pub displays: Vec<DisplayInfo>,
    /// The komorebi that was running, as its binary reported.
    pub komorebi: Vec<String>,
    pub komorebi_log_level: Option<String>,
    pub events: usize,
    pub log_lines: usize,
    pub incidents: usize,
    /// Whether the video was verified to be on the same clock as the logs.
    pub sync_quality: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Bucket {
    pub start_s: f64,
    pub end_s: f64,
    pub events: usize,
    /// Events that are not echoes of komorebi's own moves.
    pub real_events: usize,
    pub log_errors: usize,
    pub log_warnings: usize,
    pub incidents: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct OverviewData {
    pub session: SessionInfo,
    /// The regions the user marked, verbatim.
    pub bugs: Vec<crate::annotations::AnnotationView>,
    /// Ranked, most worth looking at first.
    pub findings: Vec<Finding>,
    pub findings_total: usize,
    pub histogram: Vec<Bucket>,
    pub incidents: Vec<IncidentView>,
}

impl Analysis {
    /// The whole recording at a glance.
    pub fn overview(&self, limit: Option<usize>) -> eyre::Result<OverviewData> {
        let bugs = self.bugs()?;
        let refs: Vec<&Annotation> = bugs.iter().collect();
        let ranked = self.rank(self.findings()?, &refs);
        let total = ranked.len();

        Ok(OverviewData {
            session: self.session_info(),
            bugs: bugs.iter().map(|bug| bug.view(&self.manifest)).collect(),
            findings: ranked
                .into_iter()
                .take(limit.unwrap_or(usize::MAX))
                .collect(),
            findings_total: total,
            histogram: self.histogram(60),
            incidents: self.incidents(),
        })
    }

    /// What happened to the recording environment: displays, sleep, lock, restarts.
    #[must_use]
    pub fn incidents(&self) -> Vec<IncidentView> {
        let Ok(text) = std::fs::read_to_string(self.session.path(INCIDENTS_FILE)) else {
            return Vec::new();
        };

        text.lines()
            .enumerate()
            .filter_map(|(index, line)| {
                let incident: Incident = serde_json::from_str(line).ok()?;
                Some(IncidentView {
                    number: index as u64 + 1,
                    video_s: self.video_s(incident.mach_ns),
                    kind: incident.kind,
                    detail: incident.detail,
                    display: incident.display,
                })
            })
            .collect()
    }

    /// The facts about the session an investigator wants before anything else.
    #[must_use]
    pub fn session_info(&self) -> SessionInfo {
        let manifest = &self.manifest;
        let mut displays: BTreeMap<u32, DisplayInfo> = BTreeMap::new();
        for entry in &manifest.displays {
            let info = displays
                .entry(entry.result.display.id)
                .or_insert(DisplayInfo {
                    id: entry.result.display.id,
                    width_px: entry.result.width_px,
                    height_px: entry.result.height_px,
                    segments: 0,
                });
            info.segments += 1;
        }

        SessionInfo {
            dir: self.session.dir.display().to_string(),
            status: manifest.status.clone(),
            started_utc: crate::timeline::format_wall_us(manifest.start.wall_us),
            duration_s: manifest.video_span().1,
            recorder_version: manifest.recorder_version.clone(),
            macos_version: manifest.macos_version.clone(),
            displays: displays.into_values().collect(),
            komorebi: manifest
                .komorebi
                .versions
                .iter()
                .map(|version| {
                    format!(
                        "{} {} {}",
                        version.version,
                        version.branch.as_deref().unwrap_or(""),
                        version
                            .commit
                            .as_deref()
                            .map_or("", |commit| &commit[..commit.len().min(10)])
                    )
                    .trim()
                    .to_string()
                })
                .collect(),
            komorebi_log_level: manifest
                .komorebi
                .log_level
                .as_ref()
                .map(|level| level.level.clone()),
            events: self.events().len(),
            log_lines: self.log().lines.len(),
            incidents: self.incidents().len(),
            sync_quality: manifest.sync_quality.clone(),
            warnings: manifest.warnings.clone(),
        }
    }

    /// Everything the detectors found, in time order and unranked.
    pub fn findings(&self) -> eyre::Result<Vec<Finding>> {
        let mut found = Vec::new();
        self.find_state_problems(&mut found)?;
        self.find_event_problems(&mut found);
        self.find_log_problems(&mut found);
        self.find_incidents(&mut found);
        found.sort_by_key(|finding| (finding.start_mach_ns, finding.kind.clone()));
        Ok(found)
    }

    fn finding(
        &self,
        kind: &str,
        severity: Severity,
        summary: String,
        start_mach_ns: u64,
        end_mach_ns: u64,
    ) -> Finding {
        Finding {
            kind: kind.to_string(),
            severity,
            score: 0,
            summary,
            start_video_s: self.video_s(start_mach_ns),
            end_video_s: self.video_s(end_mach_ns.max(start_mach_ns)),
            start_mach_ns,
            end_mach_ns: end_mach_ns.max(start_mach_ns),
            refs: Vec::new(),
            window_ids: Vec::new(),
            persisted_to_end: false,
            bugs: Vec::new(),
            next: None,
        }
    }

    /// The end of the recording on the master clock.
    fn session_end_ns(&self) -> u64 {
        crate::analysis::timespec::session_end_ns(&self.manifest)
    }

    fn find_state_problems(&self, found: &mut Vec<Finding>) -> eyre::Result<()> {
        let end = self.session_end_ns();
        let dir = self.session.dir.display().to_string();

        // violations: when each appeared and when it went away
        type Key = (String, Vec<u64>, Option<u64>, Option<String>);
        struct Open {
            violation: invariants::Violation,
            start_ns: u64,
            seq: u64,
        }
        let mut open: HashMap<Key, Open> = HashMap::new();
        let mut closed: Vec<(Open, u64)> = Vec::new();

        // windows' hidden flags and focus, to find flapping and oscillation
        let mut hidden_before: HashMap<u64, bool> = HashMap::new();
        let mut flips: HashMap<u64, Vec<(u64, u64)>> = HashMap::new();
        let mut exes: HashMap<u64, String> = HashMap::new();
        let mut focus_series: Vec<(u64, u64, Option<u64>)> = Vec::new();

        let mut last_state_id = None;
        for event in self.events() {
            let Some(state_id) = event.state_id else {
                continue;
            };
            if last_state_id == Some(state_id) {
                continue;
            }
            last_state_id = Some(state_id);
            let snapshot = self.snapshot(state_id)?;

            let violations = invariants::check(&snapshot);
            let mut current: HashMap<Key, invariants::Violation> = HashMap::new();
            for violation in violations {
                let mut ids = violation.window_ids.clone();
                ids.sort_unstable();
                let key = (
                    violation.kind.clone(),
                    ids,
                    violation.monitor_id,
                    violation.workspace.clone(),
                );
                current.entry(key).or_insert(violation);
            }

            let gone: Vec<Key> = open
                .keys()
                .filter(|key| !current.contains_key(*key))
                .cloned()
                .collect();
            for key in gone {
                if let Some(opened) = open.remove(&key) {
                    closed.push((opened, event.recv_mach_ns));
                }
            }
            for (key, violation) in current {
                open.entry(key).or_insert(Open {
                    violation,
                    start_ns: event.recv_mach_ns,
                    seq: event.seq,
                });
            }

            for (location, window) in snapshot.windows() {
                exes.entry(window.id).or_insert_with(|| window.exe.clone());
                let hidden = window.rect.as_ref().is_some_and(|rect| {
                    snapshot
                        .monitors
                        .iter()
                        .find(|monitor| monitor.id == location.monitor_id)
                        .is_some_and(|monitor| Snapshot::is_hidden_position(rect, monitor))
                });
                if let Some(before) = hidden_before.insert(window.id, hidden)
                    && before != hidden
                {
                    flips
                        .entry(window.id)
                        .or_default()
                        .push((event.recv_mach_ns, event.seq));
                }
            }
            focus_series.push((
                event.recv_mach_ns,
                event.seq,
                snapshot.focus().and_then(|focus| focus.window_id),
            ));
        }

        let mut push_violation = |opened: Open, ended: Option<u64>| {
            let persisted = ended.is_none();
            let end_ns = ended.unwrap_or(end);
            let lasted = end_ns.saturating_sub(opened.start_ns);
            // a state that was only wrong for an instant is usually a transition in progress
            let severity = if !persisted && lasted < 500_000_000 {
                match opened.violation.severity {
                    Severity::Error => Severity::Warn,
                    _ => Severity::Info,
                }
            } else {
                opened.violation.severity
            };

            let mut finding = self.finding(
                &format!("invariant:{}", opened.violation.kind),
                severity,
                format!(
                    "{}{}",
                    opened.violation.message,
                    if persisted {
                        ", and it was still so when the recording ended".to_string()
                    } else {
                        format!(", for {:.1} s", lasted as f64 / 1e9)
                    }
                ),
                opened.start_ns,
                end_ns,
            );
            finding.refs = vec![format!("event:{}", opened.seq)];
            finding.window_ids = opened.violation.window_ids.clone();
            finding.persisted_to_end = persisted;
            finding.next = Some(format!(
                "komorebi-recorder trace {dir} event:{}",
                opened.seq
            ));
            found.push(finding);
        };
        for (opened, ended) in closed {
            push_violation(opened, Some(ended));
        }
        for (_, opened) in open {
            push_violation(opened, None);
        }

        // a window parked and un-parked over and over
        for (window, times) in &flips {
            let mut best: Option<(usize, usize)> = None;
            for (start, (first, _)) in times.iter().enumerate() {
                let within = times[start..]
                    .iter()
                    .take_while(|(time, _)| time.saturating_sub(*first) <= 10_000_000_000)
                    .count();
                if within >= 4 && best.is_none_or(|(_, count)| within > count) {
                    best = Some((start, within));
                }
            }
            if let Some((start, count)) = best {
                let (first, seq) = times[start];
                let last = times[start + count - 1].0;
                let name = exes
                    .get(window)
                    .map_or_else(String::new, |exe| format!("{exe} "));
                let mut finding = self.finding(
                    "window-flapping",
                    Severity::Warn,
                    format!(
                        "{name}window {window} was parked off screen and brought back {count} times in {:.1} s",
                        last.saturating_sub(first) as f64 / 1e9
                    ),
                    first,
                    last,
                );
                finding.refs = vec![format!("event:{seq}")];
                finding.window_ids = vec![*window];
                finding.next = Some(format!("komorebi-recorder window {dir} {window}"));
                found.push(finding);
            }
        }

        // focus bouncing between two windows
        let mut start = 0;
        while start < focus_series.len() {
            let origin = focus_series[start].0;
            let mut changes = 0;
            let mut windows = BTreeSet::new();
            let mut last = None;
            let mut stop = start;
            for (index, (time, _, window)) in focus_series.iter().enumerate().skip(start) {
                if time.saturating_sub(origin) > 5_000_000_000 {
                    break;
                }
                if last.is_some() && *window != last {
                    changes += 1;
                }
                if let Some(window) = window {
                    windows.insert(*window);
                }
                last = *window;
                stop = index;
            }

            if changes >= 4 && windows.len() <= 2 {
                let (first, seq, _) = focus_series[start];
                let (end_time, _, _) = focus_series[stop];
                let names: Vec<String> = windows.iter().map(ToString::to_string).collect();
                let mut finding = self.finding(
                    "focus-oscillation",
                    Severity::Warn,
                    format!(
                        "focus bounced between window {} {changes} times in {:.1} s",
                        names.join(" and "),
                        end_time.saturating_sub(first) as f64 / 1e9
                    ),
                    first,
                    end_time,
                );
                finding.refs = vec![format!("event:{seq}")];
                finding.window_ids = windows.iter().copied().collect();
                finding.next = Some(format!("komorebi-recorder events {dir} --from event:{seq}"));
                found.push(finding);
                start = stop + 1;
            } else {
                start += 1;
            }
        }

        Ok(())
    }

    fn find_event_problems(&self, found: &mut Vec<Finding>) {
        let events = self.events();
        let pairing = self.pairing();
        let dir = self.session.dir.display().to_string();

        // bursts: twenty or more events within a second
        let mut start = 0;
        while start < events.len() {
            let mut stop = start;
            while stop + 1 < events.len()
                && events[stop + 1].recv_mach_ns - events[start].recv_mach_ns <= 1_000_000_000
            {
                stop += 1;
            }

            let count = stop - start + 1;
            if count >= 20 {
                let mut by_class: BTreeMap<String, usize> = BTreeMap::new();
                for index in start..=stop {
                    *by_class
                        .entry(format!("{:?}", pairing.classes[index].class).to_lowercase())
                        .or_default() += 1;
                }
                let breakdown: Vec<String> = by_class
                    .iter()
                    .map(|(class, count)| format!("{count} {class}"))
                    .collect();
                let mut finding = self.finding(
                    "event-storm",
                    if count >= 60 {
                        Severity::Warn
                    } else {
                        Severity::Info
                    },
                    format!(
                        "{count} events in {:.1} s ({})",
                        (events[stop].recv_mach_ns - events[start].recv_mach_ns) as f64 / 1e9,
                        breakdown.join(", ")
                    ),
                    events[start].recv_mach_ns,
                    events[stop].recv_mach_ns,
                );
                finding.refs = vec![
                    format!("event:{}", events[start].seq),
                    format!("event:{}", events[stop].seq),
                ];
                finding.next = Some(format!(
                    "komorebi-recorder events {dir} --from event:{} --to event:{} --hide-echo",
                    events[start].seq, events[stop].seq
                ));
                found.push(finding);
                start = stop + 1;
            } else {
                start += 1;
            }
        }

        // handlers that took a long time from their header to their notification
        let mut slow: Vec<(usize, f64)> = pairing
            .pairs
            .iter()
            .enumerate()
            .filter_map(|(index, pair)| pair.handler_ms.map(|ms| (index, ms)))
            .filter(|(_, ms)| *ms >= 250.0)
            .collect();
        slow.sort_by(|a, b| b.1.total_cmp(&a.1));
        for (index, ms) in slow.into_iter().take(10) {
            let event = &events[index];
            let mut finding = self.finding(
                "slow-handler",
                if ms >= 1000.0 {
                    Severity::Warn
                } else {
                    Severity::Info
                },
                format!(
                    "{} took at least {ms:.0} ms to handle: slow accessibility calls into other \
                     applications, or waiting for the lock another handler held",
                    event.info.summary()
                ),
                event.recv_mach_ns.saturating_sub((ms * 1e6) as u64),
                event.recv_mach_ns,
            );
            finding.refs = vec![format!("event:{}", event.seq)];
            finding.next = Some(format!("komorebi-recorder trace {dir} event:{}", event.seq));
            found.push(finding);
        }

        // display changes arriving together
        let monitor: Vec<usize> = (0..events.len())
            .filter(|index| events[*index].info.family == Family::Monitor)
            .collect();
        let mut start = 0;
        while start < monitor.len() {
            let mut stop = start;
            while stop + 1 < monitor.len()
                && events[monitor[stop + 1]].recv_mach_ns - events[monitor[start]].recv_mach_ns
                    <= 5_000_000_000
            {
                stop += 1;
            }
            if stop > start {
                let (first, last) = (&events[monitor[start]], &events[monitor[stop]]);
                let mut finding = self.finding(
                    "monitor-churn",
                    Severity::Warn,
                    format!(
                        "{} display notifications in {:.1} s ({})",
                        stop - start + 1,
                        (last.recv_mach_ns - first.recv_mach_ns) as f64 / 1e9,
                        first.info.summary()
                    ),
                    first.recv_mach_ns,
                    last.recv_mach_ns,
                );
                finding.refs = vec![
                    format!("event:{}", first.seq),
                    format!("event:{}", last.seq),
                ];
                finding.next = Some(format!("komorebi-recorder trace {dir} event:{}", first.seq));
                found.push(finding);
                start = stop + 1;
            } else {
                start += 1;
            }
        }

        let _ = Class::Echo;
    }

    fn find_log_problems(&self, found: &mut Vec<Finding>) {
        #[derive(Default)]
        struct Group {
            count: usize,
            first: u64,
            last: u64,
            first_line: u64,
            sample: String,
        }

        let mut groups: BTreeMap<(Level, String, String), Group> = BTreeMap::new();
        for line in &self.log().lines {
            let (Some(level), Some(wall_us)) = (line.level, line.wall_us) else {
                continue;
            };
            if !matches!(level, Level::Error | Level::Warn) {
                continue;
            }
            let Some(mach_ns) = self.clock().wall_to_mach(wall_us) else {
                continue;
            };

            // the same problem with different numbers is one problem
            let shape: String = line
                .message
                .chars()
                .map(|character| {
                    if character.is_ascii_digit() {
                        '#'
                    } else {
                        character
                    }
                })
                .collect();
            let group = groups
                .entry((level, line.target.clone().unwrap_or_default(), shape))
                .or_default();
            if group.count == 0 {
                group.first = mach_ns;
                group.first_line = line.number;
                group.sample = line.message.clone();
            }
            group.count += 1;
            group.last = mach_ns;
        }

        let dir = self.session.dir.display().to_string();
        for ((level, target, _), group) in groups {
            let mut finding = self.finding(
                if level == Level::Error {
                    "log-error"
                } else {
                    "log-warning"
                },
                if level == Level::Error {
                    Severity::Warn
                } else {
                    Severity::Info
                },
                format!(
                    "{}x {} {target}: {}",
                    group.count,
                    format!("{level:?}").to_uppercase(),
                    group.sample
                ),
                group.first,
                group.last,
            );
            finding.refs = vec![format!("log:{}", group.first_line)];
            finding.next = Some(format!(
                "komorebi-recorder code {dir} log:{}",
                group.first_line
            ));
            found.push(finding);
        }
    }

    fn find_incidents(&self, found: &mut Vec<Finding>) {
        for incident in self.incidents() {
            let mach_ns = self.manifest.mach_at_video_seconds(incident.video_s);
            let severity = match incident.kind.as_str() {
                "capture-failed" | "log-stream-lost" | "komorebi-exited" => Severity::Warn,
                _ => Severity::Info,
            };
            let mut finding = self.finding(
                &format!("incident:{}", incident.kind),
                severity,
                format!(
                    "{}{}: {}",
                    incident.kind,
                    incident
                        .display
                        .map_or_else(String::new, |display| format!(" (display {display})")),
                    incident.detail
                ),
                mach_ns,
                mach_ns,
            );
            finding.refs = vec![format!("incident:{}", incident.number)];
            found.push(finding);
        }
    }

    /// Rank findings: errors above warnings above notes, and anything in or just before a bug
    /// region above everything outside one.
    pub fn rank(&self, mut findings: Vec<Finding>, bugs: &[&Annotation]) -> Vec<Finding> {
        for finding in &mut findings {
            let base = match finding.severity {
                Severity::Error => 300,
                Severity::Warn => 100,
                Severity::Info => 20,
            };

            for bug in bugs {
                let (start, end) = (bug.start_mach_ns, bug.end_or_session_end(&self.manifest));
                if finding.start_mach_ns <= end && finding.end_mach_ns >= start {
                    finding.bugs.push(bug.id.clone());
                    finding.score += 1000;
                } else if finding.end_mach_ns < start && start - finding.end_mach_ns <= PRE_ROLL_NS
                {
                    finding.bugs.push(bug.id.clone());
                    finding.score += 400;
                }
            }

            finding.score += base;
            if finding.persisted_to_end {
                finding.score += 50;
            }
        }

        findings.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then(a.start_mach_ns.cmp(&b.start_mach_ns))
        });
        findings
    }

    /// Counts per time bucket: a picture of when things happened.
    #[must_use]
    pub fn histogram(&self, buckets: usize) -> Vec<Bucket> {
        let duration = self.manifest.video_span().1.max(1.0);
        let width = (duration / buckets.max(1) as f64).max(0.5);
        let count = (duration / width).ceil() as usize;
        let mut out: Vec<Bucket> = (0..count)
            .map(|index| Bucket {
                start_s: index as f64 * width,
                end_s: (index + 1) as f64 * width,
                events: 0,
                real_events: 0,
                log_errors: 0,
                log_warnings: 0,
                incidents: 0,
            })
            .collect();

        let place = |seconds: f64| -> Option<usize> {
            (seconds >= 0.0).then(|| ((seconds / width) as usize).min(count.saturating_sub(1)))
        };

        for (index, event) in self.events().iter().enumerate() {
            if let Some(slot) =
                place(self.video_s(event.recv_mach_ns)).and_then(|slot| out.get_mut(slot))
            {
                slot.events += 1;
                if self.pairing().classes[index].class != Class::Echo {
                    slot.real_events += 1;
                }
            }
        }
        for line in &self.log().lines {
            let (Some(level), Some(mach_ns)) = (
                line.level,
                line.wall_us
                    .and_then(|wall_us| self.clock().wall_to_mach(wall_us)),
            ) else {
                continue;
            };
            if let Some(slot) = place(self.video_s(mach_ns)).and_then(|slot| out.get_mut(slot)) {
                match level {
                    Level::Error => slot.log_errors += 1,
                    Level::Warn => slot.log_warnings += 1,
                    _ => {}
                }
            }
        }
        for incident in self.incidents() {
            if let Some(slot) = place(incident.video_s).and_then(|slot| out.get_mut(slot)) {
                slot.incidents += 1;
            }
        }

        out
    }

    /// The user's bug regions.
    pub fn bugs(&self) -> eyre::Result<Vec<Annotation>> {
        Ok(crate::annotations::load(&self.session.dir)?
            .annotations
            .into_iter()
            .filter(|annotation| annotation.kind == Kind::Bug)
            .collect())
    }
}

/// `00:12.345` for a finding's start.
#[must_use]
pub fn when(finding: &Finding) -> String {
    timecode::format(finding.start_video_s)
}
