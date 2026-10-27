//! Human-readable rendering of query results. Everything here is a pure function of its inputs so
//! the output format is unit-testable; `--json` bypasses this module entirely.

use crate::analysis::answers::DiffData;
use crate::analysis::answers::EventsData;
use crate::analysis::answers::StateData;
use crate::analysis::brief::BriefData;
use crate::analysis::brief::RegionBrief;
use crate::analysis::changes::ChangesData;
use crate::analysis::code::CodeData;
use crate::analysis::history::HistoryData;
use crate::analysis::invariants::Severity;
use crate::analysis::invariants::Violation;
use crate::analysis::overview::Finding;
use crate::analysis::overview::OverviewData;
use crate::analysis::search::SearchData;
use crate::analysis::sheet::SheetData;
use crate::analysis::state::Container;
use crate::analysis::state::Window;
use crate::analysis::trace::TraceData;
use crate::analysis::trace::TraceLine;
use crate::analysis::views::EventView;
use crate::annotations::AnnotationView;
use crate::annotations::Kind;
use crate::timecode;
use crate::timeline::Bucket;
use crate::timeline::Located;
use crate::timeline::Record;
use crate::timeline::SOURCE_EVENTS;
use crate::timeline::SOURCE_KOMOREBI_LOG;
use crate::timeline::SOURCE_RECORDER;
use std::collections::BTreeMap;
use std::fmt::Write;

/// The order sources are listed in: komorebi's own view first, then the OS trace from the
/// daemon outwards to the apps.
fn source_rank(source: &str) -> (u8, String) {
    let rank = match source {
        // what happened to the recording environment frames everything else
        SOURCE_RECORDER => 0,
        SOURCE_KOMOREBI_LOG => 1,
        SOURCE_EVENTS => 2,
        "oslog:komorebi" => 3,
        "oslog:system" => 4,
        _ if source.starts_with("oslog:app:") => 5,
        _ => 6,
    };
    (rank, source.to_string())
}

/// Group records by source, in display order.
#[must_use]
pub fn group_by_source(records: &[Record]) -> Vec<(String, Vec<&Record>)> {
    let mut groups: BTreeMap<(u8, String), Vec<&Record>> = BTreeMap::new();
    for record in records {
        groups
            .entry(source_rank(&record.entry.source))
            .or_default()
            .push(record);
    }

    groups
        .into_iter()
        .map(|((_, source), records)| (source, records))
        .collect()
}

/// Render `at` / `range` results. `center_s` adds a time-relative-to-the-query column.
#[must_use]
pub fn records_text(
    heading: &str,
    records: &[Record],
    center_s: Option<f64>,
    limit_per_source: Option<usize>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{heading} - {} records", records.len());

    if records.is_empty() {
        let _ = writeln!(out, "(nothing was recorded in this span)");
        return out;
    }

    for (source, group) in group_by_source(records) {
        let _ = writeln!(out, "\n== {source} ({}) ==", group.len());
        let shown = limit_per_source.map_or(group.len(), |limit| limit.min(group.len()));

        for record in group.iter().take(shown) {
            let time = timecode::format(record.entry.video_s);
            match center_s {
                Some(center) => {
                    let _ = writeln!(
                        out,
                        "{time} ({:+.3}s) {}",
                        record.entry.video_s - center,
                        record.text
                    );
                }
                None => {
                    let _ = writeln!(out, "{time} {}", record.text);
                }
            }
        }

        if shown < group.len() {
            let _ = writeln!(
                out,
                "... {} more {source} records (raise --limit or narrow with --source/--app)",
                group.len() - shown
            );
        }
    }

    out
}

/// Render a `summary`.
#[must_use]
pub fn summary_text(buckets: &[Bucket], by_source: bool) -> String {
    let mut out = String::new();
    if buckets.is_empty() {
        out.push_str("(nothing was recorded)\n");
        return out;
    }

    for bucket in buckets {
        let _ = write!(out, "{} ", timecode::format(bucket.start_s));

        if by_source {
            let counts = bucket
                .counts
                .iter()
                .map(|(source, count)| format!("{source}={count}"))
                .collect::<Vec<_>>()
                .join(" ");
            let _ = writeln!(out, "{counts}");
        } else {
            let sum = |predicate: &dyn Fn(&str) -> bool| -> u64 {
                bucket
                    .counts
                    .iter()
                    .filter(|(source, _)| predicate(source))
                    .map(|(_, count)| *count)
                    .sum()
            };
            let _ = write!(
                out,
                "log={} events={} os={}",
                sum(&|source| source == SOURCE_KOMOREBI_LOG),
                sum(&|source| source == SOURCE_EVENTS),
                sum(&|source| source.starts_with("oslog:")),
            );

            // something happened to the recording environment in this second: make it stand out
            let incidents = sum(&|source| source == SOURCE_RECORDER);
            if incidents > 0 {
                let _ = write!(out, " recorder={incidents}");
            }
            let _ = writeln!(out);
        }

        if let Some(first) = &bucket.first_log {
            let _ = writeln!(out, "    first: {first}");
        }

        if let Some(last) = &bucket.last_log
            && bucket.first_log.as_ref() != Some(last)
        {
            let _ = writeln!(out, "    last:  {last}");
        }
    }

    out
}

/// Render a `locate`.
#[must_use]
pub fn locate_text(located: &Located) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", located.description);
    let _ = writeln!(
        out,
        "video time: {} ({:.3}s){}",
        timecode::format(located.video_s),
        located.video_s,
        if located.in_video {
            ""
        } else {
            "  (outside the recorded video)"
        }
    );

    if let Some(wall) = &located.wall_utc {
        let _ = writeln!(out, "utc:        {wall}");
    }

    for display in &located.displays {
        let _ = writeln!(
            out,
            "  display {}: {} at {} ({:.3}s)",
            display.display,
            display.file,
            timecode::format(display.file_s),
            display.file_s
        );
    }

    for display in &located.without_video {
        let _ = writeln!(
            out,
            "  display {display}: no video at this moment (unplugged, asleep or its stream had stopped; see the recorder source)"
        );
    }

    out
}

/// Render one annotation: for a bug, the region and, verbatim, what the user said is wrong.
#[must_use]
pub fn annotation_text(view: &AnnotationView) -> String {
    let mut out = String::new();
    let displays = if view.displays.is_empty() {
        "any display".to_string()
    } else {
        format!(
            "display {}",
            view.displays
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let _ = writeln!(
        out,
        "{} {}  {} -> {} ({:.3}s)  {displays}  by {}",
        view.kind,
        view.id,
        timecode::format(view.start_video_s),
        timecode::format(view.end_video_s),
        view.end_video_s - view.start_video_s,
        view.author
    );

    let indent = |text: &str| text.lines().collect::<Vec<_>>().join("\n    ");
    if !view.title.is_empty() {
        let _ = writeln!(out, "  title: {}", view.title);
    }
    if !view.text.trim().is_empty() {
        let label = match view.kind {
            Kind::Bug => "what goes wrong",
            Kind::Note => "note",
            Kind::Finding => "finding",
        };
        let _ = writeln!(out, "  {label}:\n    {}", indent(&view.text));
    }
    if let Some(expected) = &view.expected {
        let _ = writeln!(out, "  expected:\n    {}", indent(expected));
    }
    if view.open_ended {
        let _ = writeln!(
            out,
            "  open-ended: no end was set, so it runs to the end of the recording"
        );
    }
    if view.needs_description {
        let _ = writeln!(
            out,
            "  needs a description: add one with `annotate <session> edit {} --text ...`",
            view.id
        );
    }
    if let Some(about) = &view.about {
        let _ = writeln!(out, "  about: {about}");
    }
    if !view.refs.is_empty() {
        let refs: Vec<String> = view.refs.iter().map(ToString::to_string).collect();
        let _ = writeln!(out, "  evidence: {}", refs.join(", "));
    }

    out
}

fn rect_text(rect: &Option<crate::analysis::state::Rect>) -> String {
    rect.as_ref()
        .map_or_else(|| "rect unknown".to_string(), |rect| rect.describe())
}

fn window_line(window: &Window, marker: &str) -> String {
    format!(
        "{marker} window {} {} \"{}\"  {}",
        window.id,
        window.exe,
        window.title,
        rect_text(&window.rect)
    )
}

fn container_lines(out: &mut String, index: usize, container: &Container, focused: bool) {
    for (tab, window) in container.windows.iter().enumerate() {
        let marker = if tab == 0 {
            format!("{index:>3}{}", if focused { "*" } else { " " })
        } else {
            "    ".to_string()
        };
        let front = if container.windows.len() > 1 && tab == container.focused {
            " (front tab)"
        } else {
            ""
        };
        let _ = writeln!(out, "      {}{front}", window_line(window, &marker));
    }
}

fn violation_lines(out: &mut String, violations: &[Violation]) {
    for violation in violations {
        let severity = match violation.severity {
            Severity::Error => "error",
            Severity::Warn => "warn ",
            Severity::Info => "info ",
        };
        let _ = writeln!(
            out,
            "  {severity} {}: {}",
            violation.kind, violation.message
        );
    }
}

/// Render the answer to `state`.
#[must_use]
pub fn state_text(data: &StateData) -> String {
    let mut out = String::new();
    let age = data.origin.age_ms.map_or_else(String::new, |age| {
        if age < 1000.0 {
            format!(", {age:.0} ms before")
        } else {
            format!(", {:.1} s before", age / 1000.0)
        }
    });
    let source = match (data.origin.source.as_str(), data.origin.seq) {
        ("event", Some(seq)) => format!("as of event #{seq}{age}"),
        _ => format!("the state when recording began{age}"),
    };
    let _ = writeln!(out, "state at {}  ({source})", data.at.timecode);
    if data.origin.age_ms.is_some_and(|age| age > 5000.0) {
        let _ = writeln!(
            out,
            "note: nothing was reported for a while, and komorebi can change state without a \
             notification, so this may be out of date"
        );
    }

    if let Some(focus) = &data.focus {
        let _ = writeln!(
            out,
            "focus: window {} on monitor {} workspace {}",
            focus
                .window_id
                .map_or_else(|| "(none)".to_string(), |id| id.to_string()),
            focus.monitor_id,
            focus.workspace
        );
    }
    if data.paused {
        let _ = writeln!(out, "komorebi is PAUSED");
    }

    for monitor in &data.monitors {
        let _ = writeln!(
            out,
            "\nmonitor {} \"{}\"  {}",
            monitor.id,
            monitor.device,
            monitor.size.describe()
        );

        for (index, workspace) in monitor.workspaces.iter().enumerate() {
            let showing = data.workspaces.iter().any(|summary| {
                summary.monitor_id == monitor.id
                    && summary.name == workspace.name
                    && summary.showing
            });
            let _ = writeln!(
                out,
                "  workspace {}{}  {}{}",
                workspace.name,
                if showing { " [showing]" } else { "" },
                workspace.layout,
                if workspace.layer == "Floating" {
                    "  [floating layer]"
                } else {
                    ""
                },
            );
            let _ = index;

            for (container_index, container) in workspace.containers.iter().enumerate() {
                container_lines(
                    &mut out,
                    container_index,
                    container,
                    showing && container_index == workspace.focused,
                );
            }
            if let Some(monocle) = &workspace.monocle {
                let _ = writeln!(out, "      monocle:");
                container_lines(&mut out, 0, monocle, true);
            }
            if let Some(window) = &workspace.maximized {
                let _ = writeln!(out, "{}", window_line(window, "      maximized"));
            }
            for window in &workspace.floating {
                let _ = writeln!(out, "{}", window_line(window, "      floating"));
            }
        }
    }

    let hidden: Vec<String> = data
        .workspaces
        .iter()
        .filter(|summary| {
            !summary.showing
                && !data.monitors.iter().any(|monitor| {
                    monitor.id == summary.monitor_id
                        && monitor
                            .workspaces
                            .iter()
                            .any(|workspace| workspace.name == summary.name)
                })
        })
        .map(|summary| format!("{} ({})", summary.name, summary.windows))
        .collect();
    if !hidden.is_empty() {
        let _ = writeln!(
            out,
            "\nnot showing (windows): {}  (--all to list them)",
            hidden.join(", ")
        );
    }

    for found in &data.matches {
        let _ = writeln!(
            out,
            "\nwindow {} {} \"{}\"\n  where:    {}\n  really at: {}{}",
            found.window.id,
            found.window.exe,
            found.window.title,
            found.location.describe(),
            rect_text(&found.window.rect),
            if found.hidden_position {
                "  (parked off screen)"
            } else {
                ""
            },
        );
        if let Some(layout) = &found.layout_rect {
            let _ = writeln!(out, "  layout says: {}", layout.describe());
        }
    }

    if data.violations.is_empty() {
        let _ = writeln!(out, "\nnothing looks wrong in this state");
    } else {
        let _ = writeln!(out, "\nwrong in this state:");
        violation_lines(&mut out, &data.violations);
    }

    out
}

/// Render the answer to `diff`.
#[must_use]
pub fn diff_text(data: &DiffData) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "from {}  to {}", data.from.timecode, data.to.timecode);
    if data.changes.is_empty() {
        let _ = writeln!(out, "no difference between the two states");
    }
    for change in &data.changes {
        let _ = writeln!(out, "  {}", change.text);
    }
    if !data.violations_appeared.is_empty() {
        let _ = writeln!(out, "\nwrong now, and not before:");
        violation_lines(&mut out, &data.violations_appeared);
    }
    if !data.violations_resolved.is_empty() {
        let _ = writeln!(out, "\nwrong before, and not now:");
        violation_lines(&mut out, &data.violations_resolved);
    }
    out
}

/// Render the answer to `events`.
#[must_use]
pub fn events_text(data: &EventsData, verbose: bool) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} of {} events{}",
        data.matching,
        data.total,
        data.subscriber
            .as_ref()
            .map_or_else(String::new, |name| format!(
                " (recorded as subscriber {name})"
            ))
    );

    for event in &data.events {
        let handler = event.handler.as_ref().map_or_else(String::new, |handler| {
            let ms = handler
                .handler_ms
                .map_or_else(String::new, |ms| format!(" {ms:.0} ms"));
            let problems = if handler.errors + handler.warnings > 0 {
                format!(" {}E/{}W", handler.errors, handler.warnings)
            } else {
                String::new()
            };
            format!("  handler{ms}{problems}")
        });
        let state = match event.state_changed {
            Some(true) => "  state changed",
            Some(false) => "  state unchanged",
            None => "",
        };
        let _ = writeln!(
            out,
            "#{:<5} {}  {:<9}  {}{handler}{state}",
            event.seq,
            timecode::format(event.video_s),
            format!("{:?}", event.class).to_lowercase(),
            event.summary,
        );
        if verbose {
            let _ = writeln!(out, "         {}", event.reason);
        }
    }

    out
}

/// Render the answer to `code`.
#[must_use]
pub fn code_text(data: &CodeData) -> String {
    let mut out = String::new();
    if let Some(warning) = &data.skew.warning {
        let _ = writeln!(out, "WARNING: {warning}\n");
    }

    for line in &data.lines {
        let time = line.video_s.map_or_else(String::new, |seconds| {
            format!("  {}", timecode::format(seconds))
        });
        let _ = writeln!(
            out,
            "log:{}{time}  {}  {}\n  {}",
            line.line,
            line.level.as_deref().unwrap_or(""),
            line.target.as_deref().unwrap_or(""),
            line.message,
        );

        match &line.source {
            Some(source) => {
                let note = match source.confidence {
                    crate::analysis::catalog::Confidence::Exact => "exact".to_string(),
                    crate::analysis::catalog::Confidence::Likely => "likely".to_string(),
                    crate::analysis::catalog::Confidence::Ambiguous => format!(
                        "ambiguous: {} other statement(s) read the same",
                        source.alternatives.len()
                    ),
                    crate::analysis::catalog::Confidence::Generic => {
                        "generic: the statement only prints a value, so this says little"
                            .to_string()
                    }
                };
                let _ = writeln!(
                    out,
                    "  -> {}  in {}  ({note}){}",
                    source.statement.location(),
                    source.statement.qualified_function(),
                    if line.file_changed {
                        "  [this file differs from the recorded komorebi]"
                    } else {
                        ""
                    }
                );
                for other in &source.alternatives {
                    let _ = writeln!(
                        out,
                        "     or {}  in {}",
                        other.location(),
                        other.qualified_function()
                    );
                }
            }
            None => {
                let _ = writeln!(out, "  -> no statement in the indexed source writes this");
            }
        }

        if let Some(snippet) = &line.snippet {
            for (number, text) in &snippet.lines {
                let marker = if *number == snippet.line { ">" } else { " " };
                let _ = writeln!(out, "     {number:>5} {marker} {text}");
            }
        }
        if let Some(role) = &line.role {
            let _ = writeln!(
                out,
                "  role: {role}{}",
                if line.changes_state_silently {
                    "  [CHANGES STATE WITHOUT A NOTIFICATION]"
                } else {
                    ""
                }
            );
        }
        out.push('\n');
    }

    for statement in &data.statements {
        let _ = writeln!(
            out,
            "{}  {}  in {}\n  {} {}",
            statement.location(),
            statement.level,
            statement.qualified_function(),
            statement.target,
            statement.template
        );
    }

    out
}

fn trace_line_text(out: &mut String, line: &TraceLine) {
    let time = line.video_s.map_or_else(String::new, |seconds| {
        format!(" {}", timecode::format(seconds))
    });
    let _ = writeln!(
        out,
        "  log:{:<6}{time} {:<5} {}{}",
        line.line,
        line.level.as_deref().unwrap_or(""),
        if line.spans.is_empty() {
            String::new()
        } else {
            format!("[{}] ", line.spans)
        },
        line.message
    );
    if let Some(source) = &line.source {
        let _ = writeln!(
            out,
            "          from {} in {}{}{}",
            source.location,
            source.function,
            if source.file_changed {
                "  [file differs from the recorded komorebi]"
            } else {
                ""
            },
            if line.changes_state_silently {
                "  [changes state without a notification]"
            } else {
                ""
            }
        );
    }
}

/// Render the answer to `trace`.
#[must_use]
pub fn trace_text(data: &TraceData) -> String {
    let mut out = String::new();
    let event = &data.event;
    let _ = writeln!(
        out,
        "event #{}  received {}  {}\n  {} ({})",
        event.seq,
        timecode::format(event.video_s),
        event.summary,
        format!("{:?}", event.class).to_lowercase(),
        event.reason
    );
    if let Some(handler) = &event.handler {
        let _ = writeln!(
            out,
            "  handler logged {} line(s) from {}, began {}{}",
            handler.lines,
            timecode::format(handler.started_video_s),
            handler.handler_ms.map_or_else(String::new, |ms| format!(
                "{ms:.0} ms before it was received"
            )),
            if handler.errors + handler.warnings > 0 {
                format!(
                    ", {} error(s), {} warning(s)",
                    handler.errors, handler.warnings
                )
            } else {
                String::new()
            }
        );
    }

    if !data.handler.is_empty() {
        let _ = writeln!(out, "\nwhat the handler logged:");
        for line in &data.handler {
            trace_line_text(&mut out, line);
        }
    }
    if !data.ambient.is_empty() {
        let _ = writeln!(out, "\nlogged by other threads meanwhile:");
        for line in &data.ambient {
            trace_line_text(&mut out, line);
        }
    }

    if data.changes.is_empty() {
        let _ = writeln!(out, "\nthe state did not change");
    } else {
        let _ = writeln!(out, "\nwhat changed, and who did it:");
        for traced in &data.changes {
            let attribution = &traced.attribution;
            let _ = writeln!(
                out,
                "  {}\n      -> {} ({}): {}",
                traced.change.text, attribution.actor, attribution.confidence, attribution.reason
            );
            if !attribution.evidence.is_empty() {
                let lines: Vec<String> = attribution
                    .evidence
                    .iter()
                    .map(|line| format!("log:{line}"))
                    .collect();
                let _ = writeln!(out, "         evidence: {}", lines.join(", "));
            }
        }
    }

    if !data.violations_appeared.is_empty() {
        let _ = writeln!(out, "\nwrong after this event, and not before:");
        violation_lines(&mut out, &data.violations_appeared);
    }
    if !data.violations_resolved.is_empty() {
        let _ = writeln!(out, "\nwrong before this event, and not after:");
        violation_lines(&mut out, &data.violations_resolved);
    }
    if !data.notes.is_empty() {
        let _ = writeln!(out, "\nnotes:");
        for note in &data.notes {
            let _ = writeln!(out, "  - {note}");
        }
    }

    out
}

/// Render the answer to `window`.
#[must_use]
pub fn history_text(data: &HistoryData) -> String {
    let mut out = String::new();
    let names: Vec<String> = data
        .windows
        .iter()
        .map(|window| format!("{} \"{}\" (window {})", window.exe, window.title, window.id))
        .collect();
    let _ = writeln!(out, "'{}' matched: {}", data.query, names.join("; "));

    for entry in &data.entries {
        let place = entry
            .location
            .as_ref()
            .map_or_else(|| "gone".to_string(), |location| location.describe());
        let _ = writeln!(
            out,
            "#{:<5} {}  window {}  {}\n         after: {}  {}{}",
            entry.seq,
            timecode::format(entry.video_s),
            entry.window_id,
            entry.event,
            place,
            rect_text(&entry.rect),
            if entry.hidden {
                "  (parked off screen)"
            } else {
                ""
            },
        );
        for change in &entry.changes {
            let _ = writeln!(out, "         - {change}");
        }
        if entry.changes.is_empty() && entry.about_window {
            let _ = writeln!(
                out,
                "         - the event was about this window; nothing about it changed"
            );
        }
    }

    if data.matching > data.entries.len() {
        let _ = writeln!(out, "... {} more", data.matching - data.entries.len());
    }

    out
}

fn event_line(event: &EventView) -> String {
    let handler = event.handler.as_ref().map_or_else(String::new, |handler| {
        let ms = handler
            .handler_ms
            .map_or_else(String::new, |ms| format!(" {ms:.0} ms"));
        let problems = if handler.errors + handler.warnings > 0 {
            format!(" {}E/{}W", handler.errors, handler.warnings)
        } else {
            String::new()
        };
        format!("  handler{ms}{problems}")
    });
    let state = match event.state_changed {
        Some(true) => "  state changed",
        _ => "",
    };
    format!(
        "#{:<5} {}  {:<9}  {}{handler}{state}",
        event.seq,
        timecode::format(event.video_s),
        format!("{:?}", event.class).to_lowercase(),
        event.summary
    )
}

fn finding_line(finding: &Finding) -> String {
    let severity = match finding.severity {
        Severity::Error => "error",
        Severity::Warn => "warn ",
        Severity::Info => "info ",
    };
    let bugs = if finding.bugs.is_empty() {
        String::new()
    } else {
        "  [in or just before a bug region]".to_string()
    };
    format!(
        "{severity} {}  {}  {}{bugs}",
        timecode::format(finding.start_video_s),
        finding.kind,
        finding.summary
    )
}

fn region_text(out: &mut String, region: &RegionBrief) {
    let _ = writeln!(
        out,
        "\nlooking at {} to {} (the region, from a few seconds before it)",
        region.from.timecode, region.to.timecode
    );

    let classes: Vec<String> = region
        .events
        .by_class
        .iter()
        .map(|(class, count)| format!("{count} {class}"))
        .collect();
    let _ = writeln!(
        out,
        "events: {} ({}{})",
        region.events.total,
        classes.join(", "),
        if region.events.echoes_hidden > 0 {
            format!(
                "; {} echoes of komorebi's own moves not listed",
                region.events.echoes_hidden
            )
        } else {
            String::new()
        }
    );
    for event in &region.events.listed {
        let _ = writeln!(out, "  {}", event_line(event));
    }
    if region.events.truncated {
        let _ = writeln!(out, "  ... more: use `events` with --from/--to to page");
    }

    if !region.suspects.is_empty() {
        let _ = writeln!(out, "\nmost worth tracing first:");
        for (index, suspect) in region.suspects.iter().enumerate() {
            let _ = writeln!(
                out,
                "  {}. event #{} {} (score {}): {}\n     {}",
                index + 1,
                suspect.event.seq,
                suspect.event.summary,
                suspect.score,
                suspect.reasons.join("; "),
                suspect.next
            );
        }
    }

    if !region.findings.is_empty() {
        let _ = writeln!(out, "\nwhat looked wrong or unusual:");
        for finding in &region.findings {
            let _ = writeln!(out, "  {}", finding_line(finding));
        }
    }

    for (label, state) in [
        ("as it began", &region.state_at_start),
        ("as it ended", &region.state_at_end),
    ] {
        match state {
            Some(state) => {
                let _ = writeln!(
                    out,
                    "\nkomorebi's state {label} ({}): {}",
                    state.at.timecode, state.summary
                );
                if state.violations.is_empty() {
                    let _ = writeln!(out, "  nothing looks wrong");
                } else {
                    violation_lines(out, &state.violations);
                }
            }
            None => {
                let _ = writeln!(out, "\nkomorebi's state {label}: not recorded");
            }
        }
    }

    if !region.changes.is_empty() {
        let _ = writeln!(
            out,
            "\nwhat changed between them ({} total):",
            region.changes_total
        );
        for change in &region.changes {
            let _ = writeln!(out, "  {}", change.text);
        }
    }

    if !region.incidents.is_empty() {
        let _ = writeln!(out, "\nwhat happened to the machine nearby:");
        for incident in &region.incidents {
            let _ = writeln!(
                out,
                "  {}  {}{}: {}",
                timecode::format(incident.video_s),
                incident.kind,
                incident
                    .display
                    .map_or_else(String::new, |display| format!(" (display {display})")),
                incident.detail
            );
        }
    }
}

/// Render the answer to `brief`: what the user says is wrong comes first, word for word.
#[must_use]
pub fn brief_text(data: &BriefData) -> String {
    let mut out = String::new();

    if data.bugs.is_empty() {
        let _ = writeln!(out, "NO BUG REGION WAS MARKED in this recording.\n");
    }
    for bug in &data.bugs {
        let _ = writeln!(out, "BUG, as the user reported it:");
        out.push_str(&annotation_text(&bug.bug));
    }

    let session = &data.session;
    let displays: Vec<String> = session
        .displays
        .iter()
        .map(|display| {
            format!(
                "{} ({}x{}, {} segment(s))",
                display.id, display.width_px, display.height_px, display.segments
            )
        })
        .collect();
    let _ = writeln!(
        out,
        "\nsession {}\n  {} s of video, displays: {}\n  komorebi: {}{}\n  {} events, {} log lines, {} incidents; video sync: {}",
        session.dir,
        session.duration_s as i64,
        if displays.is_empty() {
            "none".to_string()
        } else {
            displays.join(", ")
        },
        if session.komorebi.is_empty() {
            "version unknown".to_string()
        } else {
            session.komorebi.join("; ")
        },
        session
            .komorebi_log_level
            .as_ref()
            .map_or_else(String::new, |level| format!(", logging at {level}")),
        session.events,
        session.log_lines,
        session.incidents,
        session.sync_quality
    );
    for warning in &session.warnings {
        let _ = writeln!(out, "  warning: {warning}");
    }
    if let Some(warning) = &data.skew.warning {
        let _ = writeln!(out, "\nWARNING: {warning}");
    }

    for bug in &data.bugs {
        let _ = writeln!(out, "\n=== {} ===", bug.bug.id);
        region_text(&mut out, &bug.region);
        let _ = writeln!(out, "\nnext:");
        for command in &bug.commands {
            let _ = writeln!(out, "  {command}");
        }
    }
    if let Some(region) = &data.whole_session {
        let _ = writeln!(out, "\n=== the whole recording ===");
        region_text(&mut out, region);
    }

    if !data.findings_recorded.is_empty() {
        let _ = writeln!(out, "\nconclusions already recorded:");
        for finding in &data.findings_recorded {
            out.push_str(&annotation_text(finding));
        }
    }
    if !data.notes.is_empty() {
        let _ = writeln!(out, "\nnotes:");
        for note in &data.notes {
            out.push_str(&annotation_text(note));
        }
    }

    if !data.next.is_empty() {
        let _ = writeln!(out, "\n");
        for line in &data.next {
            let _ = writeln!(out, "{line}");
        }
    }

    out
}

/// Render the answer to `overview`.
#[must_use]
pub fn overview_text(data: &OverviewData) -> String {
    let mut out = String::new();
    let session = &data.session;
    let _ = writeln!(
        out,
        "session {}: {} s of video, {} events, {} log lines, {} incidents",
        session.dir,
        session.duration_s as i64,
        session.events,
        session.log_lines,
        session.incidents
    );

    for bug in &data.bugs {
        let _ = writeln!(out, "\nbug region:");
        out.push_str(&annotation_text(bug));
    }

    let _ = writeln!(
        out,
        "\nfindings, most worth looking at first ({} of {}):",
        data.findings.len(),
        data.findings_total
    );
    for finding in &data.findings {
        let _ = writeln!(out, "  {}", finding_line(finding));
        if let Some(next) = &finding.next {
            let _ = writeln!(out, "         {next}");
        }
    }

    let peak = data
        .histogram
        .iter()
        .map(|bucket| bucket.events)
        .max()
        .unwrap_or(0)
        .max(1);
    let _ = writeln!(out, "\nwhen things happened (events | errors | incidents):");
    for bucket in &data.histogram {
        if bucket.events + bucket.log_errors + bucket.incidents == 0 {
            continue;
        }
        let bar = "#".repeat((bucket.events * 30).div_ceil(peak));
        let _ = writeln!(
            out,
            "  {} {:<30} {:>4} {:>3} {:>2}",
            timecode::format(bucket.start_s),
            bar,
            bucket.events,
            bucket.log_errors,
            bucket.incidents
        );
    }

    out
}

/// Render the answer to `search`.
#[must_use]
pub fn search_text(data: &SearchData) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} hit(s) for '{}' in {}",
        data.matching,
        data.query,
        data.searched.join(", ")
    );
    for hit in &data.hits {
        let _ = writeln!(
            out,
            "{}  {:<14} {:<22} {}",
            timecode::format(hit.video_s),
            hit.source,
            hit.reference,
            hit.text.lines().next().unwrap_or("")
        );
    }
    out
}

/// Render the answer to `changes`.
#[must_use]
pub fn changes_text(data: &ChangesData) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} frames compared on display(s) {} from {} to {}: {} stretch(es) of change",
        data.frames_examined,
        data.displays
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        timecode::format(data.from_video_s),
        timecode::format(data.to_video_s),
        data.spans.len()
    );
    for span in &data.spans {
        let events = if span.events.is_empty() {
            String::new()
        } else {
            format!(
                "  events {}",
                span.events
                    .iter()
                    .map(|seq| format!("#{seq}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        let _ = writeln!(
            out,
            "display {}  {} - {} ({:.2} s, {} frames)  up to {:.1}% of the screen  box {:.0}%,{:.0}% {:.0}%x{:.0}%{events}",
            span.display,
            timecode::format(span.start_video_s),
            timecode::format(span.end_video_s),
            span.end_video_s - span.start_video_s,
            span.frames,
            span.peak_share * 100.0,
            span.bbox[0] * 100.0,
            span.bbox[1] * 100.0,
            span.bbox[2] * 100.0,
            span.bbox[3] * 100.0,
        );
    }
    for gap in &data.gaps {
        let _ = writeln!(
            out,
            "display {} has no video from {} to {}",
            gap.display,
            timecode::format(gap.from_video_s),
            timecode::format(gap.to_video_s)
        );
    }
    out
}

/// Render the answer to `sheet`.
#[must_use]
pub fn sheet_text(data: &SheetData) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{}  ({}x{}, {} frames)",
        data.path,
        data.width,
        data.height,
        data.frames.len()
    );
    for skipped in &data.skipped {
        let _ = writeln!(
            out,
            "no frame: display {} at {}: {}",
            skipped.display,
            timecode::format(skipped.video_s),
            skipped.reason
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeline::TimelineEntry;

    fn record(source: &str, video_s: f64, text: &str) -> Record {
        Record {
            entry: TimelineEntry {
                video_s,
                mach_ns: 0,
                wall_utc: None,
                source: source.to_string(),
                pid: None,
                file: String::new(),
                offset: 0,
                len: 0,
                line: 1,
                seq: None,
            },
            text: text.to_string(),
        }
    }

    #[test]
    fn groups_in_display_order() {
        let records = vec![
            record("oslog:app:Ghostty", 1.0, "g"),
            record("oslog:system", 1.0, "s"),
            record(SOURCE_EVENTS, 1.0, "e"),
            record("oslog:komorebi", 1.0, "k"),
            record(SOURCE_KOMOREBI_LOG, 1.0, "l"),
            record("oslog:app:Brave", 1.0, "b"),
            record(SOURCE_RECORDER, 1.0, "r"),
        ];

        let order: Vec<String> = group_by_source(&records)
            .into_iter()
            .map(|(source, _)| source)
            .collect();
        assert_eq!(
            order,
            [
                SOURCE_RECORDER,
                SOURCE_KOMOREBI_LOG,
                SOURCE_EVENTS,
                "oslog:komorebi",
                "oslog:system",
                "oslog:app:Brave",
                "oslog:app:Ghostty",
            ]
        );
    }

    #[test]
    fn at_output_shows_offsets_and_truncates_per_source() {
        let records = vec![
            record(SOURCE_KOMOREBI_LOG, 12.4, "line a"),
            record(SOURCE_KOMOREBI_LOG, 12.5, "line b"),
            record(SOURCE_KOMOREBI_LOG, 12.6, "line c"),
        ];
        let text = records_text("00:12.500 +-1s", &records, Some(12.5), Some(2));

        assert!(text.contains("3 records"));
        assert!(text.contains("== komorebi-log (3) =="));
        assert!(text.contains("00:12.400 (-0.100s) line a"), "{text}");
        assert!(text.contains("00:12.600 (+0.100s)") || text.contains("... 1 more"));
        assert!(text.contains("... 1 more komorebi-log records"));
        assert!(!text.contains("line c"));
    }

    #[test]
    fn empty_results_say_so() {
        assert!(records_text("x", &[], None, None).contains("nothing was recorded"));
    }

    #[test]
    fn summary_collapses_oslog_sources_unless_asked() {
        let bucket = Bucket {
            start_s: 12.0,
            end_s: 13.0,
            counts: BTreeMap::from([
                (SOURCE_KOMOREBI_LOG.to_string(), 4),
                (SOURCE_EVENTS.to_string(), 2),
                ("oslog:app:Ghostty".to_string(), 30),
                ("oslog:system".to_string(), 5),
            ]),
            first_log: Some("first".to_string()),
            last_log: Some("last".to_string()),
        };

        let collapsed = summary_text(std::slice::from_ref(&bucket), false);
        assert!(
            collapsed.contains("00:12.000 log=4 events=2 os=35"),
            "{collapsed}"
        );
        assert!(collapsed.contains("first: first"));
        assert!(collapsed.contains("last:  last"));

        assert!(!collapsed.contains("recorder="), "no incidents, no column");

        let detailed = summary_text(&[bucket], true);
        assert!(detailed.contains("oslog:app:Ghostty=30"));
    }

    #[test]
    fn summary_flags_a_second_in_which_the_environment_changed() {
        let bucket = Bucket {
            start_s: 40.0,
            end_s: 41.0,
            counts: BTreeMap::from([
                (SOURCE_KOMOREBI_LOG.to_string(), 1),
                (SOURCE_RECORDER.to_string(), 2),
            ]),
            first_log: None,
            last_log: None,
        };

        let text = summary_text(&[bucket], false);
        assert!(
            text.contains("00:40.000 log=1 events=0 os=0 recorder=2"),
            "{text}"
        );
    }
}
