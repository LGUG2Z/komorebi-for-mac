//! Rebuilding a session whose recorder died before it could finish.
//!
//! While recording, everything is written so that a crash, a `kill -9`, a power loss or a
//! panic loses as little as possible: video is fragmented mp4 (playable up to the last fragment),
//! the frame log and every raw source are flushed continuously, and the manifest is checkpointed
//! every few seconds with `status: "recording"`. Recovery turns that checkpoint into a normal,
//! queryable session: it closes the segments that were still open using the frame log, sets a
//! stop time from the latest evidence on disk, and marks the session `recovered`.

use crate::capture::EndReason;
use crate::clock::ClockMap;
use crate::session::Moment;
use crate::session::STATUS_RECORDING;
use crate::session::STATUS_RECOVERED;
use crate::session::Session;
use crate::sources::komorebi_events::EventRow;
use color_eyre::eyre;
use std::path::Path;

/// What the frame log of a segment says about it. Rows whose index is `-` are keep-alive
/// re-submissions of the last frame on an idle display: they count towards how long the segment
/// lasted but are not new frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FrameLog {
    pub frames: u64,
    pub first_pts_mach_ns: Option<u64>,
    pub last_pts_mach_ns: Option<u64>,
}

/// Read a `frames-*.csv`, ignoring the header and any torn or malformed row (the last row of a
/// log that was being written when the recorder died).
#[must_use]
pub fn read_frame_log(path: &Path) -> FrameLog {
    read_frame_log_until(path, u64::MAX)
}

/// Like [`read_frame_log`], but ignoring rows later than `until_pts_mach_ns`: the part of the
/// log that never made it into the video.
#[must_use]
pub fn read_frame_log_until(path: &Path, until_pts_mach_ns: u64) -> FrameLog {
    let Ok(text) = std::fs::read_to_string(path) else {
        return FrameLog::default();
    };

    let mut log = FrameLog::default();
    for line in text.lines().skip(1) {
        let mut fields = line.split(',');
        let (Some(index), Some(pts), Some(received), None) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };

        let (Ok(pts), Ok(_)) = (pts.parse::<u64>(), received.parse::<u64>()) else {
            continue;
        };

        if pts > until_pts_mach_ns {
            break;
        }

        if index != "-" {
            // a keep-alive can never be the first row, so the first row is a real frame
            log.frames += 1;
            log.first_pts_mach_ns.get_or_insert(pts);
        }
        log.last_pts_mach_ns = Some(pts);
    }

    log
}

/// One frame at the capture frame rate cap, used to give the last recovered frame a duration.
const FRAME_NS: u64 = 33_333_333;

/// Recover `session` if it was left unfinished, asking AVFoundation how long each video file
/// really is. Returns notes describing what was done (empty if the session was already complete).
pub fn recover(session: &Session) -> eyre::Result<Vec<String>> {
    recover_with(session, &crate::capture::sck::media_duration_s)
}

/// [`recover`] with the file-duration lookup supplied, so it can be tested without video files.
pub fn recover_with(
    session: &Session,
    duration_of: &dyn Fn(&Path) -> Option<f64>,
) -> eyre::Result<Vec<String>> {
    let mut manifest = session.manifest()?;
    if manifest.status != STATUS_RECORDING {
        return Ok(Vec::new());
    }

    let mut notes = Vec::new();
    let mut results: Vec<_> = manifest
        .displays
        .iter()
        .map(|entry| entry.result.clone())
        .collect();

    for result in &mut results {
        if result.end_reason.is_some() {
            continue;
        }

        let frames_path = session.path(&result.frames_file);
        let whole_log = read_frame_log(&frames_path);
        let t0 = result.video_t0_mach_ns.or(whole_log.first_pts_mach_ns);

        // The video file is the truth about how much was kept: fragments are cut on a timer, and
        // the frame log is flushed on another, so either can be ahead of the other. Without a
        // readable file, fall back to the log.
        let file_duration = duration_of(&session.path(&result.file)).filter(|d| *d > 0.0);
        let end = match (t0, file_duration) {
            (Some(t0), Some(duration)) => Some(t0 + (duration * 1e9) as u64),
            _ => whole_log.last_pts_mach_ns.map(|last| last + FRAME_NS),
        };

        let log = end.map_or(whole_log, |end| read_frame_log_until(&frames_path, end));
        result.frames = log.frames;
        result.video_t0_mach_ns = t0;
        result.video_end_mach_ns = end;
        result.end_reason = Some(EndReason::Interrupted);

        notes.push(match (log.frames, file_duration) {
            (0, _) => format!("{}: no frames had been recorded", result.file),
            (frames, Some(duration)) => format!(
                "{}: recovered {frames} frames, {duration:.1}s of video from the file",
                result.file
            ),
            (frames, None) => format!(
                "{}: {frames} frames are in the frame log but the video file could not be read, so it may be unplayable",
                result.file
            ),
        });
    }

    manifest.apply_segments(results);

    // the recording ended no earlier than the latest thing we can see on disk
    let clock = ClockMap::new(manifest.anchors.clone());
    let mut latest = manifest
        .anchors
        .iter()
        .map(|a| a.mach_ns)
        .max()
        .unwrap_or(manifest.start.mach_ns);
    latest = latest.max(manifest.video_end_mach_ns.unwrap_or(0));
    if let Ok(text) = std::fs::read_to_string(session.path("events.jsonl")) {
        let last_event = text
            .lines()
            .filter_map(|line| serde_json::from_str::<EventRow>(line).ok())
            .map(|row| row.recv_mach_ns)
            .max();
        latest = latest.max(last_event.unwrap_or(0));
    }

    let wall_us = clock.mach_to_wall(latest).unwrap_or(manifest.start.wall_us);
    manifest.stop = Some(Moment {
        mach_ns: latest,
        wall_us,
    });

    manifest.status = STATUS_RECOVERED.to_string();
    manifest.warnings.push(
        "the recorder stopped unexpectedly (crash, kill or power loss); this session was recovered \
         from what was on disk, so its end is approximate and a little of the end may be missing"
            .to_string(),
    );
    notes.push("session marked as recovered".to_string());

    session.write_manifest(&manifest)?;
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::Codec;
    use crate::capture::DisplayDescriptor;
    use crate::capture::DisplayResult;
    use crate::capture::LatencyStats;
    use crate::clock::Anchor;
    use crate::session::Manifest;
    use chrono::Utc;

    fn temp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "komorebi-recorder-recovery-{name}-{}",
            std::process::id()
        ))
    }

    fn live_segment(display: u32, segment: u32, t0: u64) -> DisplayResult {
        DisplayResult {
            display: DisplayDescriptor {
                id: display,
                frame: [0.0, 0.0, 100.0, 100.0],
                width_points: 100,
                height_points: 100,
                asleep: false,
            },
            segment,
            scale: 1.0,
            width_px: 100,
            height_px: 100,
            codec: Codec::Hevc,
            file: crate::capture::video_file_name(display, segment),
            frames_file: crate::capture::frames_file_name(display, segment),
            video_t0_mach_ns: Some(t0),
            video_end_mach_ns: None,
            end_reason: None,
            frames: 3,
            dropped: 0,
            idle_callbacks: 0,
            latency: LatencyStats::default(),
            error: None,
        }
    }

    #[test]
    fn a_frame_log_with_a_torn_last_row_is_read_up_to_the_tear() {
        let dir = temp("csv");
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("frames-2.csv");
        // what `kill -9` left behind in a real recording: the last row cut mid-field
        std::fs::write(
            &path,
            "index,pts_mach_ns,received_mach_ns\n0,1000,1010\n1,2000,2010\n2,3000",
        )
        .expect("write");

        let log = read_frame_log(&path);
        assert_eq!(log.frames, 2, "the torn row is not counted");
        assert_eq!(log.first_pts_mach_ns, Some(1000));
        assert_eq!(log.last_pts_mach_ns, Some(2000));

        assert_eq!(
            read_frame_log(&dir.join("missing.csv")),
            FrameLog::default()
        );
    }

    #[test]
    fn keep_alive_rows_extend_the_segment_but_are_not_frames() {
        let dir = temp("keepalive");
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("frames-1.csv");
        std::fs::write(
            &path,
            "index,pts_mach_ns,received_mach_ns\n0,1000,1010\n1,2000,2010\n-,2600,2600\n-,3100,3100\n",
        )
        .expect("write");

        let log = read_frame_log(&path);
        assert_eq!(log.frames, 2, "only the real frames");
        assert_eq!(log.first_pts_mach_ns, Some(1000));
        assert_eq!(
            log.last_pts_mach_ns,
            Some(3100),
            "the segment lasted until the last keep-alive"
        );
    }

    #[test]
    fn an_interrupted_session_is_closed_out_from_its_frame_logs() {
        let dir = temp("session");
        let _ = std::fs::remove_dir_all(&dir);
        let session = Session::create(Some(dir.clone()), Utc::now()).expect("create");

        // a checkpoint taken 10s in: one finished segment, one still open when the recorder died
        let start = Moment {
            mach_ns: 1_000_000_000_000,
            wall_us: 1_800_000_000_000_000,
        };
        let mut manifest = Manifest::new(
            start,
            vec![
                Anchor {
                    wall_us: start.wall_us,
                    mach_ns: start.mach_ns,
                },
                Anchor {
                    wall_us: start.wall_us + 5_000_000,
                    mach_ns: start.mach_ns + 5_000_000_000,
                },
            ],
        );
        let mut finished = live_segment(1, 1, 1_000_100_000_000);
        finished.video_end_mach_ns = Some(1_003_000_000_000);
        finished.end_reason = Some(EndReason::DisplayDisconnected);
        manifest.apply_segments(vec![finished, live_segment(2, 1, 1_000_200_000_000)]);
        assert_eq!(manifest.status, STATUS_RECORDING);
        session.write_manifest(&manifest).expect("write");

        // the open segment's frame log, as left on disk (torn last row)
        std::fs::write(
            session.path("frames-2.csv"),
            "index,pts_mach_ns,received_mach_ns\n0,1000200000000,1\n1,1007000000000,2\n2,1008000000000,3\n3,1009000",
        )
        .expect("csv");
        // an event shortly after the last frame
        std::fs::write(
            session.path("events.jsonl"),
            "{\"seq\":1,\"recv_wall_us\":1,\"recv_mach_ns\":1009500000000}\n",
        )
        .expect("events");

        // the video file on disk holds 7.5s of the 8s the frame log covers
        let notes = recover_with(&session, &|path: &Path| {
            path.ends_with("video-2.mp4").then_some(7.5)
        })
        .expect("recover");
        assert!(
            notes
                .iter()
                .any(|note| note.contains("video-2.mp4: recovered 2 frames, 7.5s")),
            "{notes:?}"
        );

        let recovered = session.manifest().expect("manifest");
        assert_eq!(recovered.status, STATUS_RECOVERED);
        assert!(
            recovered
                .warnings
                .iter()
                .any(|w| w.contains("stopped unexpectedly"))
        );

        let open = recovered
            .displays
            .iter()
            .find(|entry| entry.result.display.id == 2)
            .expect("segment");
        assert_eq!(open.result.end_reason, Some(EndReason::Interrupted));
        // the 3rd frame (at +8s) is past the 7.5s of video that survived, so it is not counted
        assert_eq!(open.result.frames, 2);
        assert_eq!(
            open.result.video_end_mach_ns,
            Some(1_000_200_000_000 + 7_500_000_000)
        );
        assert!(open.end_s.is_some());

        let closed = recovered
            .displays
            .iter()
            .find(|entry| entry.result.display.id == 1)
            .expect("segment");
        assert_eq!(
            closed.result.end_reason,
            Some(EndReason::DisplayDisconnected),
            "left alone"
        );

        // the stop time is the latest evidence: the event, not the stale 5s anchor
        assert_eq!(recovered.stop.expect("stop").mach_ns, 1_009_500_000_000);
        assert!(recovered.video_end_mach_ns.is_some());

        // and a second pass does nothing
        assert!(
            recover_with(&session, &|_: &Path| None)
                .expect("again")
                .is_empty()
        );
    }

    #[test]
    fn an_unreadable_video_file_falls_back_to_the_frame_log_and_says_so() {
        let dir = temp("unreadable");
        let _ = std::fs::remove_dir_all(&dir);
        let session = Session::create(Some(dir), Utc::now()).expect("create");
        let start = Moment {
            mach_ns: 1_000_000_000_000,
            wall_us: 1_800_000_000_000_000,
        };
        let mut manifest = Manifest::new(
            start,
            vec![Anchor {
                wall_us: start.wall_us,
                mach_ns: start.mach_ns,
            }],
        );
        manifest.apply_segments(vec![live_segment(2, 1, 1_000_100_000_000)]);
        session.write_manifest(&manifest).expect("write");
        std::fs::write(
            session.path("frames-2.csv"),
            "index,pts_mach_ns,received_mach_ns\n0,1000100000000,1\n1,1002100000000,2\n",
        )
        .expect("csv");

        let notes = recover_with(&session, &|_: &Path| None).expect("recover");
        assert!(
            notes.iter().any(|note| note.contains("could not be read")),
            "{notes:?}"
        );

        let recovered = session.manifest().expect("manifest");
        assert_eq!(recovered.displays[0].result.frames, 2);
        assert_eq!(
            recovered.displays[0].result.video_end_mach_ns,
            Some(1_002_100_000_000 + FRAME_NS)
        );
    }

    #[test]
    fn a_complete_session_is_left_alone() {
        let dir = temp("complete");
        let _ = std::fs::remove_dir_all(&dir);
        let session = Session::create(Some(dir), Utc::now()).expect("create");
        let start = Moment {
            mach_ns: 1,
            wall_us: 1,
        };
        let mut manifest = Manifest::new(start, vec![]);
        manifest.status = crate::session::STATUS_COMPLETE.to_string();
        session.write_manifest(&manifest).expect("write");

        assert!(
            recover_with(&session, &|_: &Path| None)
                .expect("recover")
                .is_empty()
        );
        assert_eq!(session.manifest().expect("manifest"), manifest);
    }
}
