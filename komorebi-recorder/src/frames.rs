//! Extracting still frames from a session's video so the exact moment can be looked at.
//!
//! `ffmpeg` is used only here, never for capture. Frames in the mp4 only exist where the screen
//! changed, so the frame returned for a timecode is the most recent one at or before it - which
//! is exactly what was on screen at that moment.

use crate::session::Manifest;
use crate::session::Session;
use crate::timecode;
use color_eyre::eyre;
use color_eyre::eyre::WrapErr;
use serde::Serialize;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExtractedFrame {
    pub display: u32,
    pub video_s: f64,
    /// The time within the display's own file.
    pub file_s: f64,
    pub path: PathBuf,
}

/// Where the frame for `video_s` on `display` goes.
#[must_use]
pub fn frame_path(session: &Session, display: u32, video_s: f64) -> PathBuf {
    session.path(format!("frames/frame-{display}-t{video_s:09.3}.png"))
}

/// Whether `ffmpeg` can be run.
#[must_use]
pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// A display that was asked for but had no video at the requested moment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Skipped {
    pub display: u32,
    pub video_s: f64,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Extraction {
    pub frames: Vec<ExtractedFrame>,
    pub skipped: Vec<Skipped>,
}

/// Extract a frame at each of `times` from every display (or just `display`) that has video at
/// that moment. A display that was unplugged, asleep or between segments at that moment is
/// reported in `skipped` rather than failing the others.
pub fn extract(
    session: &Session,
    manifest: &Manifest,
    times: &[f64],
    display: Option<u32>,
) -> eyre::Result<Extraction> {
    if !ffmpeg_available() {
        eyre::bail!(
            "ffmpeg was not found on PATH; install it (e.g. `brew install ffmpeg`, or use \
             `nix develop`, which provides it) to extract frames"
        );
    }

    let ids: Vec<u32> = manifest
        .display_ids()
        .into_iter()
        .filter(|id| display.is_none_or(|wanted| wanted == *id))
        .collect();

    if ids.is_empty() {
        eyre::bail!(match display {
            Some(id) => format!("display {id} has no recorded video in this session"),
            None => "this session has no recorded video".to_string(),
        });
    }

    std::fs::create_dir_all(session.path("frames"))?;
    let (_, duration) = manifest.video_span();
    let mut extraction = Extraction {
        frames: Vec::new(),
        skipped: Vec::new(),
    };

    for &video_s in times {
        if video_s < 0.0 || video_s > duration + 0.5 {
            eyre::bail!(
                "{} is outside the recorded video (00:00.000 to {})",
                timecode::format(video_s),
                timecode::format(duration)
            );
        }

        let before = extraction.frames.len();
        for &id in &ids {
            let Some(entry) = manifest.segment_at(id, video_s) else {
                extraction.skipped.push(Skipped {
                    display: id,
                    video_s,
                    reason: "no video for this display at that moment (it was unplugged, asleep, \
                             or its stream had stopped)"
                        .to_string(),
                });
                continue;
            };

            let file_s = (video_s - entry.offset_s).max(0.0);
            let path = frame_path(session, id, video_s);
            let output = Command::new("ffmpeg")
                .args(["-y", "-v", "error", "-ss", &format!("{file_s:.3}"), "-i"])
                .arg(session.path(&entry.result.file))
                .args(["-frames:v", "1"])
                .arg(&path)
                .output()
                .wrap_err("could not run ffmpeg")?;

            if !output.status.success() || !path.is_file() {
                eyre::bail!(
                    "ffmpeg could not extract a frame at {} from {}: {}",
                    timecode::format(file_s),
                    entry.result.file,
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }

            extraction.frames.push(ExtractedFrame {
                display: id,
                video_s,
                file_s,
                path,
            });
        }

        if extraction.frames.len() == before {
            eyre::bail!(
                "no display has video at {}: {}",
                timecode::format(video_s),
                extraction
                    .skipped
                    .iter()
                    .filter(|skipped| skipped.video_s == video_s)
                    .map(|skipped| format!("display {}", skipped.display))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    Ok(extraction)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_paths_sort_chronologically() {
        let session = Session {
            dir: PathBuf::from("/s"),
        };
        let a = frame_path(&session, 2, 9.5);
        let b = frame_path(&session, 2, 12.25);
        assert_eq!(a, PathBuf::from("/s/frames/frame-2-t00009.500.png"));
        assert_eq!(b, PathBuf::from("/s/frames/frame-2-t00012.250.png"));
        assert!(a < b);
    }
}
