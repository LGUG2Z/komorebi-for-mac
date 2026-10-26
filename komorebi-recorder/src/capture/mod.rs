//! Screen capture. The macOS implementation lives in [`sck`]; everything here is the plain-data
//! description of what was captured, plus the two traits ([`CaptureBackend`], [`RunningCapture`])
//! that keep everything that decides *what* to record free of Objective-C. That separation is
//! what makes display hotplug, sleep and wake testable without unplugging monitors.

use color_eyre::eyre;
use parking_lot::Mutex;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

pub mod sck;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    #[default]
    Hevc,
    H264,
}

impl std::fmt::Display for Codec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Codec::Hevc => f.write_str("hevc"),
            Codec::H264 => f.write_str("h264"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Upper bound on the frame rate; ScreenCaptureKit only emits frames when content changes.
    pub fps: u32,
    /// Capture at native pixel resolution rather than logical points.
    pub retina: bool,
    pub codec: Codec,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            fps: 30,
            retina: false,
            codec: Codec::default(),
        }
    }
}

/// A display that can be captured.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayDescriptor {
    pub id: u32,
    /// `[x, y, width, height]` in global logical points.
    pub frame: [f64; 4],
    pub width_points: i64,
    pub height_points: i64,
    /// Attached but asleep. A sleeping display cannot be captured and its stream stalls, so it is
    /// treated as absent until it wakes.
    #[serde(default)]
    pub asleep: bool,
}

impl DisplayDescriptor {
    /// Whether the display's pixel size differs from `other`'s. A change of size (a resolution
    /// change, or the same display id coming back as a different monitor) invalidates a running
    /// capture, whereas a change of position (rearranging displays) does not.
    #[must_use]
    pub fn size_differs(&self, other: &Self) -> bool {
        self.width_points != other.width_points || self.height_points != other.height_points
    }
}

/// How far ScreenCaptureKit frame timestamps lag behind the moment the callback observed them.
/// If the timestamps were not on the host clock this would be wildly off, so it doubles as the
/// clock-unification check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LatencyStats {
    pub samples: u64,
    pub min_ns: i64,
    pub max_ns: i64,
    pub mean_ns: i64,
}

impl LatencyStats {
    #[must_use]
    pub fn looks_like_host_clock(&self) -> bool {
        // Frames are stamped at (or marginally ahead of) the display refresh that produced them:
        // measured -7.5 ms at the early end and ~200 ms at the late end (the first frame, while
        // the encoder spins up). A *different* clock would be off by seconds or more, so these
        // bounds only have to separate "same clock" from "not the same clock".
        self.samples == 0 || (self.min_ns >= -100_000_000 && self.max_ns < 2_000_000_000)
    }
}

/// Why a capture segment ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EndReason {
    /// The recording was stopped normally.
    Stopped,
    /// The display went away (unplugged, undocked, lid closed).
    DisplayDisconnected,
    /// The display went to sleep; a new segment starts when it wakes.
    DisplayAsleep,
    /// ScreenCaptureKit stopped the stream (sleep, lock, a display reconfiguration, ...).
    StreamError,
    /// The display's size changed, so the segment was closed and a new one begun.
    Reconfigured,
    /// The machine slept and woke. ScreenCaptureKit streams are not trusted across a sleep (they
    /// can stall without reporting anything), so each is replaced with a fresh one.
    Resumed,
    /// The recorder died before finalizing it; recovered from the fragmented file afterwards.
    Interrupted,
}

impl std::fmt::Display for EndReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            EndReason::Stopped => "stopped",
            EndReason::DisplayDisconnected => "display disconnected",
            EndReason::DisplayAsleep => "display asleep",
            EndReason::StreamError => "stream error",
            EndReason::Reconfigured => "display reconfigured",
            EndReason::Resumed => "machine resumed from sleep",
            EndReason::Interrupted => "interrupted",
        })
    }
}

/// The file name of a segment's video. The first segment of a display has the plain name so the
/// common single-segment session looks the same as it always did.
#[must_use]
pub fn video_file_name(display: u32, segment: u32) -> String {
    if segment <= 1 {
        format!("video-{display}.mp4")
    } else {
        format!("video-{display}-{segment}.mp4")
    }
}

/// The file name of a segment's frame timestamp log.
#[must_use]
pub fn frames_file_name(display: u32, segment: u32) -> String {
    if segment <= 1 {
        format!("frames-{display}.csv")
    } else {
        format!("frames-{display}-{segment}.csv")
    }
}

/// One contiguous recording of one display: a display that is unplugged and replugged, or whose
/// stream dies, produces several of these, each a complete, playable file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayResult {
    pub display: DisplayDescriptor,
    /// 1-based, per display.
    #[serde(default = "first_segment")]
    pub segment: u32,
    pub scale: f64,
    pub width_px: usize,
    pub height_px: usize,
    pub codec: Codec,
    pub file: String,
    pub frames_file: String,
    /// Host-clock time of the first frame in the file, if any frame was received.
    pub video_t0_mach_ns: Option<u64>,
    /// Host-clock time this segment ended (frames are variable-rate, so the last frame can be
    /// earlier). `None` while it is still being recorded.
    #[serde(default)]
    pub video_end_mach_ns: Option<u64>,
    #[serde(default)]
    pub end_reason: Option<EndReason>,
    pub frames: u64,
    pub dropped: u64,
    /// Callbacks that carried no new content. A healthy stream on a static screen still makes
    /// these, so a segment with frames but none of these, or vice versa, tells you which way a
    /// "frozen video" report went wrong.
    #[serde(default)]
    pub idle_callbacks: u64,
    pub latency: LatencyStats,
    pub error: Option<String>,
}

fn first_segment() -> u32 {
    1
}

/// Live counters, readable from any thread while a capture is running.
#[derive(Debug, Default)]
pub struct CaptureStats {
    pub frames: AtomicU64,
    pub dropped: AtomicU64,
    /// `0` until the first frame arrives.
    pub first_pts_mach_ns: AtomicU64,
    pub last_pts_mach_ns: AtomicU64,
    /// Callbacks of any kind, including "idle" ones that carry no new content. ScreenCaptureKit
    /// keeps calling while a stream is healthy even when the screen is not changing, which makes
    /// this the liveness signal a static display otherwise lacks.
    pub callbacks: AtomicU64,
    /// Callbacks that reported no new content (status idle/blank/suspended).
    pub idle_callbacks: AtomicU64,
    /// When the last callback of any kind arrived (`0` for none yet).
    pub last_callback_mach_ns: AtomicU64,
    /// When the first error was reported (`0` for none), so a dead stream's segment can end at
    /// the moment it died rather than whenever we noticed.
    pub error_at_mach_ns: AtomicU64,
    error: Mutex<Option<String>>,
}

impl CaptureStats {
    #[must_use]
    pub fn first_pts_mach_ns(&self) -> Option<u64> {
        match self.first_pts_mach_ns.load(Ordering::Relaxed) {
            0 => None,
            ns => Some(ns),
        }
    }

    #[must_use]
    pub fn last_callback_mach_ns(&self) -> Option<u64> {
        match self.last_callback_mach_ns.load(Ordering::Relaxed) {
            0 => None,
            ns => Some(ns),
        }
    }

    #[must_use]
    pub fn last_pts_mach_ns(&self) -> Option<u64> {
        match self.last_pts_mach_ns.load(Ordering::Relaxed) {
            0 => None,
            ns => Some(ns),
        }
    }

    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.error.lock().clone()
    }

    #[must_use]
    pub fn error_at_mach_ns(&self) -> Option<u64> {
        match self.error_at_mach_ns.load(Ordering::Relaxed) {
            0 => None,
            ns => Some(ns),
        }
    }

    /// Record the first error; later ones are consequences of it.
    pub fn set_error(&self, message: String, at_mach_ns: u64) {
        let mut error = self.error.lock();
        if error.is_none() {
            *error = Some(message);
            self.error_at_mach_ns.store(at_mach_ns, Ordering::Relaxed);
        }
    }
}

/// A capture of one display that is in progress.
pub trait RunningCapture {
    fn display_id(&self) -> u32;
    fn segment(&self) -> u32;
    fn stats(&self) -> Arc<CaptureStats>;
    /// What is known about this segment so far, without ending it (for checkpoints).
    fn partial(&self) -> DisplayResult;
    /// When this capture last showed a sign of life: any callback from ScreenCaptureKit, even an
    /// idle one, or its start time if none has arrived yet. `None` if the backend cannot tell, in
    /// which case the capture is never considered stalled.
    fn last_signal_mach_ns(&self) -> Option<u64> {
        None
    }
    /// Called several times a second. A display whose screen is not changing produces no frames,
    /// which would leave its file unflushed; this is where an idle capture keeps itself moving.
    fn keepalive(&self, _now_ns: u64) {}
    /// End the segment and finalize its file. `end_mach_ns` is when it really ended, if that is
    /// earlier than now (a stream that died).
    fn stop(self: Box<Self>, reason: EndReason, end_mach_ns: Option<u64>) -> DisplayResult;
}

/// Where displays and captures come from.
pub trait CaptureBackend {
    /// The displays that exist right now. Must be cheap: it is polled several times a second.
    fn active_displays(&self) -> eyre::Result<Vec<DisplayDescriptor>>;

    fn start(
        &self,
        display: &DisplayDescriptor,
        segment: u32,
        config: &CaptureConfig,
        dir: &Path,
    ) -> eyre::Result<Box<dyn RunningCapture>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_clock_check_accepts_measured_latencies_and_rejects_other_clocks() {
        // measured on a real display
        let measured = LatencyStats {
            samples: 229,
            min_ns: -7_506_167,
            max_ns: 196_015_958,
            mean_ns: 6_524_913,
        };
        assert!(measured.looks_like_host_clock());

        // a different clock (e.g. wall time vs mach time) is off by far more than this
        let wrong = LatencyStats {
            samples: 10,
            min_ns: 3_600_000_000_000,
            max_ns: 3_600_100_000_000,
            mean_ns: 3_600_050_000_000,
        };
        assert!(!wrong.looks_like_host_clock());

        let negative = LatencyStats {
            samples: 10,
            min_ns: -3_000_000_000,
            max_ns: -2_900_000_000,
            mean_ns: -2_950_000_000,
        };
        assert!(!negative.looks_like_host_clock());

        assert!(
            LatencyStats::default().looks_like_host_clock(),
            "no frames, nothing to doubt"
        );
    }

    #[test]
    fn segment_file_names_keep_the_first_segment_plain() {
        assert_eq!(video_file_name(2, 1), "video-2.mp4");
        assert_eq!(frames_file_name(2, 1), "frames-2.csv");
        assert_eq!(video_file_name(2, 2), "video-2-2.mp4");
        assert_eq!(frames_file_name(2, 3), "frames-2-3.csv");
        // a display id that ends in the same digits as a segment number cannot collide
        assert_ne!(video_file_name(1, 2), video_file_name(12, 1));
    }

    #[test]
    fn only_a_size_change_invalidates_a_capture() {
        let display = |x: f64, w: i64, h: i64| DisplayDescriptor {
            id: 1,
            frame: [x, 0.0, w as f64, h as f64],
            width_points: w,
            height_points: h,
            asleep: false,
        };

        assert!(!display(0.0, 1800, 1169).size_differs(&display(-1800.0, 1800, 1169)));
        assert!(display(0.0, 1800, 1169).size_differs(&display(0.0, 1512, 982)));
    }

    #[test]
    fn only_the_first_error_is_kept() {
        let stats = CaptureStats::default();
        assert_eq!(stats.error(), None);
        stats.set_error("first".into(), 100);
        stats.set_error("second".into(), 200);
        assert_eq!(stats.error().as_deref(), Some("first"));
        assert_eq!(stats.error_at_mach_ns(), Some(100));
    }

    #[test]
    fn old_manifests_without_segment_fields_still_parse() {
        let old = r#"{
            "display": {"id": 2, "frame": [0.0, 0.0, 3840.0, 1620.0], "width_points": 3840, "height_points": 1620},
            "scale": 1.0, "width_px": 3840, "height_px": 1620, "codec": "hevc",
            "file": "video-2.mp4", "frames_file": "frames-2.csv",
            "video_t0_mach_ns": 5, "frames": 3, "dropped": 0,
            "latency": {"samples": 0, "min_ns": 0, "max_ns": 0, "mean_ns": 0},
            "error": null
        }"#;
        let result: DisplayResult = serde_json::from_str(old).expect("old format parses");
        assert_eq!(result.segment, 1);
        assert_eq!(result.video_end_mach_ns, None);
        assert_eq!(result.end_reason, None);
    }
}
