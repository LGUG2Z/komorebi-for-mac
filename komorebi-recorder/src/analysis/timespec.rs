//! Naming a moment: every command that takes a time takes any of these.
//!
//! | form | meaning |
//! |---|---|
//! | `12.5`, `1:02.5`, `-1.5` | video time, see [`crate::timecode`] |
//! | `wall:2026-10-26T20:00:00Z` | a UTC time (RFC 3339) |
//! | `mach:123456789` | the recorder's master clock, in nanoseconds |
//! | `event:12`, `log:345`, `incident:2` | when that record happened |
//! | `bug:a-3f9c1e02`, `...start`, `...end` | an annotation's start or end |
//!
//! Any of them can be followed by `+3s` or `-500ms` to move it. An event or annotation can be
//! followed by `.start` or `.end` first (`bug:a-3f9c1e02.end+2s`); the default is the start.
//!
//! Resolving a name never guesses: an unknown event, a log line without a timestamp or an
//! annotation that does not exist is an error with the "not found" exit code. The one exception is
//! an annotation whose end was never set, which ends when the recording did.

use super::envelope;
use super::ids::ItemRef;
use super::ids::is_annotation_id;
use crate::clock::ClockMap;
use crate::session::Manifest;
use crate::timecode;
use chrono::DateTime;
use chrono::Utc;
use color_eyre::eyre;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Edge {
    #[default]
    Start,
    End,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Anchor {
    Video(f64),
    Wall(DateTime<Utc>),
    Mach(u64),
    Item { item: ItemRef, edge: Edge },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimeSpec {
    pub anchor: Anchor,
    /// Added to the anchor.
    pub offset_s: f64,
}

/// A moment on both clocks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolved {
    pub mach_ns: u64,
    pub video_s: f64,
}

/// What a session knows about the things a [`TimeSpec`] can point at. Implemented over the real
/// files by the analysis facade and by fakes in tests.
pub trait Lookup {
    /// When event `seq` happened. [`Edge::Start`] is when komorebi began handling it where known,
    /// [`Edge::End`] when the notification was received.
    fn event_mach_ns(&self, seq: u64, edge: Edge) -> eyre::Result<u64>;
    fn log_mach_ns(&self, line: u64) -> eyre::Result<u64>;
    fn incident_mach_ns(&self, number: u64) -> eyre::Result<u64>;
    /// An annotation's `(start, end)`; the end is `None` when it has none.
    fn annotation_span(&self, id: &str) -> eyre::Result<(u64, Option<u64>)>;
}

impl FromStr for TimeSpec {
    type Err = eyre::Report;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(envelope::usage("an empty time"));
        }

        let (head, offset_s) = split_offset(trimmed);
        Ok(Self {
            anchor: parse_anchor(head, input)?,
            offset_s,
        })
    }
}

/// Split a trailing `+3s` / `-500ms` off. The tail must end in a unit, so a `+02:00` time zone or
/// a `-` inside an id is never taken for an offset.
fn split_offset(text: &str) -> (&str, f64) {
    for (index, character) in text.char_indices().rev() {
        if index == 0 || (character != '+' && character != '-') {
            continue;
        }

        let tail = &text[index + 1..];
        if !(tail.ends_with('s') || tail.ends_with('m')) {
            continue;
        }

        if let Ok(span) = timecode::parse_span(tail) {
            let signed = if character == '-' { -span } else { span };
            return (&text[..index], signed);
        }
    }

    (text, 0.0)
}

fn parse_anchor(head: &str, original: &str) -> eyre::Result<Anchor> {
    if let Some(rest) = head.strip_prefix("wall:") {
        let time = DateTime::parse_from_rfc3339(rest).map_err(|_| {
            envelope::usage(format!(
                "'{original}': expected an RFC 3339 time like wall:2026-10-26T19:43:35.062613Z"
            ))
        })?;
        return Ok(Anchor::Wall(time.with_timezone(&Utc)));
    }

    if let Some(rest) = head.strip_prefix("mach:") {
        let ns = rest.parse().map_err(|_| {
            envelope::usage(format!(
                "'{original}': mach: takes nanoseconds, e.g. mach:123456"
            ))
        })?;
        return Ok(Anchor::Mach(ns));
    }

    let (body, edge) = match head
        .strip_suffix(".start")
        .map(|body| (body, Edge::Start))
        .or_else(|| head.strip_suffix(".end").map(|body| (body, Edge::End)))
    {
        Some((body, edge)) => (body, edge),
        None => (head, Edge::Start),
    };

    if ItemRef::has_prefix(body) || is_annotation_id(body) {
        return Ok(Anchor::Item {
            item: body.parse()?,
            edge,
        });
    }

    timecode::parse(head)
        .map(Anchor::Video)
        .map_err(|error| envelope::usage(format!("{error}")))
}

impl TimeSpec {
    pub fn resolve(&self, manifest: &Manifest, lookup: &dyn Lookup) -> eyre::Result<Resolved> {
        let base_ns = match &self.anchor {
            Anchor::Video(seconds) => manifest.mach_at_video_seconds(*seconds),
            Anchor::Mach(ns) => *ns,
            Anchor::Wall(time) => ClockMap::new(manifest.anchors.clone())
                .wall_to_mach(time.timestamp_micros())
                .ok_or_else(|| envelope::unusable("the session has no clock anchors"))?,
            Anchor::Item { item, edge } => match item {
                ItemRef::Event(seq) => lookup.event_mach_ns(*seq, *edge)?,
                ItemRef::Log(line) => lookup.log_mach_ns(*line)?,
                ItemRef::Incident(number) => lookup.incident_mach_ns(*number)?,
                ItemRef::Annotation(id) => {
                    let (start, end) = lookup.annotation_span(id)?;
                    match edge {
                        Edge::Start => start,
                        // an annotation that was never closed lasts until the recording ended
                        Edge::End => end.unwrap_or_else(|| session_end_ns(manifest)),
                    }
                }
            },
        };

        let shifted = i128::from(base_ns) + (self.offset_s * 1e9).round() as i128;
        let mach_ns = u64::try_from(shifted.max(0)).unwrap_or(u64::MAX);
        Ok(Resolved {
            mach_ns,
            video_s: manifest.video_seconds(mach_ns),
        })
    }
}

/// When the recording ended on the master clock.
#[must_use]
pub fn session_end_ns(manifest: &Manifest) -> u64 {
    manifest
        .stop
        .map(|stop| stop.mach_ns)
        .or(manifest.video_end_mach_ns)
        .unwrap_or(manifest.start.mach_ns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Anchor as ClockAnchor;
    use crate::session::Moment;

    struct Fake;

    impl Lookup for Fake {
        fn event_mach_ns(&self, seq: u64, edge: Edge) -> eyre::Result<u64> {
            match (seq, edge) {
                (1, Edge::Start) => Ok(4_000_000_000),
                (1, Edge::End) => Ok(4_500_000_000),
                _ => Err(envelope::not_found(format!("no event {seq}"))),
            }
        }

        fn log_mach_ns(&self, line: u64) -> eyre::Result<u64> {
            Ok(1_000_000_000 * line)
        }

        fn incident_mach_ns(&self, number: u64) -> eyre::Result<u64> {
            Err(envelope::not_found(format!("no incident {number}")))
        }

        fn annotation_span(&self, id: &str) -> eyre::Result<(u64, Option<u64>)> {
            match id {
                "a-00000001" => Ok((2_000_000_000, Some(3_000_000_000))),
                "a-00000002" => Ok((2_000_000_000, None)),
                _ => Err(envelope::not_found(format!("no annotation {id}"))),
            }
        }
    }

    /// Video t=0 is mach 1s; wall 1_000_000us is mach 1s; the session ended at mach 10s.
    fn manifest() -> Manifest {
        let mut manifest = Manifest::new(
            Moment {
                mach_ns: 1_000_000_000,
                wall_us: 1_000_000,
            },
            vec![ClockAnchor {
                mach_ns: 1_000_000_000,
                wall_us: 1_000_000,
            }],
        );
        manifest.video_t0_mach_ns = Some(1_000_000_000);
        manifest.stop = Some(Moment {
            mach_ns: 10_000_000_000,
            wall_us: 10_000_000,
        });
        manifest
    }

    fn resolve(text: &str) -> eyre::Result<Resolved> {
        text.parse::<TimeSpec>()?.resolve(&manifest(), &Fake)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn video_times_resolve_against_the_manifest() {
        let resolved = resolve("2.5").expect("resolve");
        assert_eq!(resolved.mach_ns, 3_500_000_000);
        assert!(close(resolved.video_s, 2.5));
        assert!(close(resolve("0:02.5").expect("clock").video_s, 2.5));
    }

    #[test]
    fn offsets_move_any_anchor() {
        assert!(close(resolve("2.5+1s").expect("plus").video_s, 3.5));
        assert!(close(resolve("2.5-500ms").expect("minus").video_s, 2.0));
        assert!(close(
            resolve("event:1.end-1s").expect("event").video_s,
            2.5
        ));
        // video t=0 is mach 1s here, so half a second before it is still on the clock
        assert!(close(
            resolve("-0.5").expect("negative video").video_s,
            -0.5
        ));
    }

    #[test]
    fn mach_and_wall_times_resolve() {
        assert!(close(
            resolve("mach:3000000000").expect("mach").video_s,
            2.0
        ));
        // wall 5s after the epoch is 4s after the anchor at wall 1s: mach 5s, video 4s
        let wall = resolve("wall:1970-01-01T00:00:05Z").expect("wall");
        assert!(close(wall.video_s, 4.0));
        // a numeric zone is not an offset
        let zoned = resolve("wall:1970-01-01T01:00:05+01:00").expect("zoned");
        assert!(close(zoned.video_s, 4.0));
        let moved = resolve("wall:1970-01-01T00:00:05Z+2s").expect("moved");
        assert!(close(moved.video_s, 6.0));
    }

    #[test]
    fn items_resolve_through_the_lookup() {
        assert!(close(resolve("event:1").expect("start").video_s, 3.0));
        assert!(close(resolve("event:1.end").expect("end").video_s, 3.5));
        assert!(close(resolve("log:7").expect("log").video_s, 6.0));
        assert!(close(resolve("bug:a-00000001").expect("bug").video_s, 1.0));
        assert!(close(
            resolve("bug:a-00000001.end").expect("bug end").video_s,
            2.0
        ));
        assert!(close(
            resolve("a-00000001.end+1s").expect("bare id").video_s,
            3.0
        ));
    }

    #[test]
    fn an_unclosed_annotation_ends_with_the_session() {
        let end = resolve("bug:a-00000002.end").expect("open-ended");
        assert_eq!(end.mach_ns, 10_000_000_000);
    }

    #[test]
    fn missing_things_are_not_found_not_guesses() {
        for text in ["event:99", "incident:1", "bug:a-0000ffff"] {
            let error = resolve(text).expect_err(text);
            assert_eq!(
                envelope::exit_kind(&error),
                envelope::ExitKind::NotFound,
                "{text}"
            );
        }
    }

    #[test]
    fn nonsense_is_a_usage_error() {
        for text in [
            "",
            "soon",
            "wall:yesterday",
            "mach:x",
            "event:one",
            "1:2:3:4",
        ] {
            let error = text
                .parse::<TimeSpec>()
                .map(|_| ())
                .expect_err(&format!("{text:?} should not parse"));
            assert_eq!(
                envelope::exit_kind(&error),
                envelope::ExitKind::Usage,
                "{text:?}"
            );
        }
    }

    #[test]
    fn times_before_the_clock_started_clamp_to_zero() {
        let resolved = resolve("mach:5-9s").expect("clamped");
        assert_eq!(resolved.mach_ns, 0);
    }
}
