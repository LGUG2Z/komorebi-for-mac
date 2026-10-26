//! Human-readable rendering of query results. Everything here is a pure function of its inputs so
//! the output format is unit-testable; `--json` bypasses this module entirely.

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
