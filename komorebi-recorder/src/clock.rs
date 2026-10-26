//! The master clock for a recording session.
//!
//! Everything in a session is expressed on the host monotonic clock (`mach_absolute_time`, in
//! nanoseconds). ScreenCaptureKit frame timestamps and the unified log's `machTimestamp` are
//! already on this clock; komorebi's own log lines (UTC wall clock) are mapped onto it with a
//! series of [`Anchor`]s sampled while recording.

use serde::Deserialize;
use serde::Serialize;
use std::sync::OnceLock;

/// `numer / denom` converts mach ticks to nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timebase {
    pub numer: u32,
    pub denom: u32,
}

impl Timebase {
    #[must_use]
    pub fn ticks_to_ns(self, ticks: u64) -> u64 {
        (u128::from(ticks) * u128::from(self.numer) / u128::from(self.denom)) as u64
    }

    #[must_use]
    pub fn ns_to_ticks(self, ns: u64) -> u64 {
        (u128::from(ns) * u128::from(self.denom) / u128::from(self.numer)) as u64
    }
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

// `libc` marks its mach bindings deprecated in favour of the `mach2` crate; these two symbols
// are all we need from libSystem, so declare them directly instead of adding a dependency
unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

#[must_use]
pub fn timebase() -> Timebase {
    static TIMEBASE: OnceLock<Timebase> = OnceLock::new();
    *TIMEBASE.get_or_init(|| {
        let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
        // SAFETY: `info` is a valid out pointer for the duration of the call
        let status = unsafe { mach_timebase_info(&raw mut info) };
        if status != 0 || info.denom == 0 {
            // every Mac that can run this has a valid timebase; 1/1 is the safe identity
            return Timebase { numer: 1, denom: 1 };
        }

        Timebase {
            numer: info.numer,
            denom: info.denom,
        }
    })
}

/// Nanoseconds on the host monotonic clock (pauses while the machine sleeps).
#[must_use]
pub fn mach_now_ns() -> u64 {
    // SAFETY: no preconditions
    let ticks = unsafe { mach_absolute_time() };
    timebase().ticks_to_ns(ticks)
}

/// Microseconds since the unix epoch on the (steppable) wall clock.
#[must_use]
pub fn wall_now_us() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid out pointer for the duration of the call
    unsafe {
        libc::clock_gettime(libc::CLOCK_REALTIME, &raw mut ts);
    }

    ts.tv_sec * 1_000_000 + ts.tv_nsec / 1_000
}

/// A paired reading of the wall and mach clocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub wall_us: i64,
    pub mach_ns: u64,
}

impl Anchor {
    /// Take a tight paired reading: several back-to-back samples, keeping the one whose wall read
    /// was bracketed by the narrowest pair of mach reads, which bounds the pairing error to the
    /// duration of a couple of syscalls.
    #[must_use]
    pub fn sample() -> Self {
        let mut best: Option<(u64, Anchor)> = None;
        for _ in 0..7 {
            let before = mach_now_ns();
            let wall_us = wall_now_us();
            let after = mach_now_ns();
            let width = after.saturating_sub(before);
            let anchor = Anchor {
                wall_us,
                mach_ns: before + width / 2,
            };

            if best.is_none_or(|(best_width, _)| width < best_width) {
                best = Some((width, anchor));
            }
        }

        best.map(|(_, anchor)| anchor).unwrap_or_else(|| Anchor {
            wall_us: wall_now_us(),
            mach_ns: mach_now_ns(),
        })
    }

    /// The wall clock minus the mach clock, in microseconds.
    fn offset_us(self) -> i128 {
        i128::from(self.wall_us) - i128::from(self.mach_ns / 1_000)
    }
}

/// How far the wall clock must outrun the mach clock between two close observations for it to
/// count as a jump. Observations are made every ~50 ms, so ordinary drift is microseconds.
const JUMP_THRESHOLD_NS: i128 = 1_000_000_000;

/// A discontinuity between the wall clock and the mach clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Jump {
    /// How far the wall clock moved beyond the mach clock, in seconds. Positive: the machine
    /// slept (mach time pauses while asleep, wall time does not) or the clock stepped forward.
    /// Negative: the wall clock was stepped back.
    pub seconds: f64,
    /// The last good pairing before the jump and the first after it.
    pub before: Anchor,
    pub after: Anchor,
}

/// Notices sleep/wake and clock steps by watching both clocks.
///
/// Feed it a fresh [`Anchor`] every loop iteration. When the two clocks disagree about how much
/// time passed since the previous observation, the machine was suspended (or the clock was
/// stepped), and the two anchors on either side of the discontinuity are exactly what
/// [`ClockMap`] needs to map wall time to mach time correctly on both sides of it.
#[derive(Debug, Clone, Copy)]
pub struct JumpDetector {
    last: Anchor,
}

impl JumpDetector {
    #[must_use]
    pub fn new(first: Anchor) -> Self {
        Self { last: first }
    }

    pub fn observe(&mut self, now: Anchor) -> Option<Jump> {
        let wall_ns = (i128::from(now.wall_us) - i128::from(self.last.wall_us)) * 1_000;
        let mach_ns = i128::from(now.mach_ns) - i128::from(self.last.mach_ns);
        let difference = wall_ns - mach_ns;
        let before = std::mem::replace(&mut self.last, now);

        (difference.abs() >= JUMP_THRESHOLD_NS).then_some(Jump {
            seconds: difference as f64 / 1e9,
            before,
            after: now,
        })
    }
}

/// Segments whose wall and mach durations disagree by more than this are treated as a clock step
/// (an NTP jump, or the machine sleeping while the mach clock was paused) rather than as drift.
const STEP_THRESHOLD_NS: i128 = 20_000_000;

/// Maps between wall-clock time and the mach clock using piecewise-linear interpolation between
/// anchors sampled during the recording.
#[derive(Debug, Clone, Default)]
pub struct ClockMap {
    anchors: Vec<Anchor>,
}

impl ClockMap {
    #[must_use]
    pub fn new(mut anchors: Vec<Anchor>) -> Self {
        anchors.sort_by_key(|anchor| anchor.mach_ns);
        anchors.dedup_by_key(|anchor| anchor.mach_ns);
        Self { anchors }
    }

    #[must_use]
    pub fn anchors(&self) -> &[Anchor] {
        &self.anchors
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }

    /// Whether the segment between two anchors contains a clock step rather than plain drift.
    fn is_step(a: Anchor, b: Anchor) -> bool {
        let wall_ns = (i128::from(b.wall_us) - i128::from(a.wall_us)) * 1_000;
        let mach_ns = i128::from(b.mach_ns) - i128::from(a.mach_ns);
        (wall_ns - mach_ns).abs() > STEP_THRESHOLD_NS
    }

    #[must_use]
    pub fn wall_to_mach(&self, wall_us: i64) -> Option<u64> {
        let (first, last) = (*self.anchors.first()?, *self.anchors.last()?);
        let extrapolate = |anchor: Anchor| {
            let mach = i128::from(wall_us) - anchor.offset_us();
            (mach * 1_000).max(0) as u64
        };

        if self.anchors.len() == 1 {
            return Some(extrapolate(first));
        }

        for pair in self.anchors.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let (lo, hi) = (a.wall_us.min(b.wall_us), a.wall_us.max(b.wall_us));
            if wall_us < lo || wall_us > hi {
                continue;
            }

            if Self::is_step(a, b) {
                // we cannot know when inside the segment the step happened, so trust whichever
                // end is closer in wall time
                let nearest = if (wall_us - a.wall_us).abs() <= (wall_us - b.wall_us).abs() {
                    a
                } else {
                    b
                };
                return Some(extrapolate(nearest));
            }

            let wall_span = i128::from(b.wall_us) - i128::from(a.wall_us);
            if wall_span == 0 {
                return Some(a.mach_ns);
            }

            let mach_span = i128::from(b.mach_ns) - i128::from(a.mach_ns);
            let into = i128::from(wall_us) - i128::from(a.wall_us);
            return Some((i128::from(a.mach_ns) + into * mach_span / wall_span).max(0) as u64);
        }

        // outside every segment: extend from whichever end is nearest in wall time
        let nearest = if (wall_us - first.wall_us).abs() <= (wall_us - last.wall_us).abs() {
            first
        } else {
            last
        };
        Some(extrapolate(nearest))
    }

    #[must_use]
    pub fn mach_to_wall(&self, mach_ns: u64) -> Option<i64> {
        let (first, last) = (*self.anchors.first()?, *self.anchors.last()?);
        let extrapolate = |anchor: Anchor| {
            let wall = i128::from(mach_ns / 1_000) + anchor.offset_us();
            wall as i64
        };

        if mach_ns <= first.mach_ns {
            return Some(extrapolate(first));
        }

        if mach_ns >= last.mach_ns {
            return Some(extrapolate(last));
        }

        for pair in self.anchors.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            if mach_ns < a.mach_ns || mach_ns > b.mach_ns {
                continue;
            }

            if Self::is_step(a, b) {
                let nearest = if mach_ns - a.mach_ns <= b.mach_ns - mach_ns {
                    a
                } else {
                    b
                };
                return Some(extrapolate(nearest));
            }

            let mach_span = i128::from(b.mach_ns) - i128::from(a.mach_ns);
            let wall_span = i128::from(b.wall_us) - i128::from(a.wall_us);
            let into = i128::from(mach_ns) - i128::from(a.mach_ns);
            return Some((i128::from(a.wall_us) + into * wall_span / mach_span) as i64);
        }

        Some(extrapolate(last))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(wall_s: i64, mach_s: u64) -> Anchor {
        Anchor {
            wall_us: wall_s * 1_000_000,
            mach_ns: mach_s * 1_000_000_000,
        }
    }

    #[test]
    fn timebase_roundtrip() {
        let tb = Timebase {
            numer: 125,
            denom: 3,
        };
        let ns = tb.ticks_to_ns(24_000_000);
        assert_eq!(ns, 1_000_000_000);
        assert_eq!(tb.ns_to_ticks(ns), 24_000_000);
    }

    #[test]
    fn mach_is_monotonic_and_wall_is_sane() {
        let a = mach_now_ns();
        let b = mach_now_ns();
        assert!(b >= a);
        // after 2020 and before 2100
        assert!(wall_now_us() > 1_577_836_800_000_000);
        assert!(wall_now_us() < 4_102_444_800_000_000);
    }

    #[test]
    fn anchor_sample_is_close_to_now() {
        let anchor = Anchor::sample();
        let now = wall_now_us();
        assert!((now - anchor.wall_us).abs() < 50_000);
    }

    #[test]
    fn interpolates_between_anchors() {
        // wall = mach + 1000s, with the wall clock running 100ppm fast over the segment
        let a = anchor(2000, 1000);
        let b = Anchor {
            wall_us: 2005 * 1_000_000 + 500,
            mach_ns: 1005 * 1_000_000_000,
        };
        let map = ClockMap::new(vec![a, b]);

        let mach = map.wall_to_mach(2_002_500_000 + 250).expect("mach");
        assert_eq!(mach, 1_002_500_000_000);

        let wall = map.mach_to_wall(1_002_500_000_000).expect("wall");
        assert_eq!(wall, 2_002_500_000 + 250);
    }

    #[test]
    fn roundtrips_within_a_microsecond() {
        let map = ClockMap::new(vec![
            anchor(2000, 1000),
            anchor(2005, 1005),
            anchor(2010, 1010),
        ]);
        for wall in [2_000_123_456, 2_004_999_999, 2_007_777_777, 2_009_000_001] {
            let mach = map.wall_to_mach(wall).expect("mach");
            let back = map.mach_to_wall(mach).expect("wall");
            assert!((back - wall).abs() <= 1, "{wall} -> {mach} -> {back}");
        }
    }

    #[test]
    fn extrapolates_outside_the_anchor_range() {
        let map = ClockMap::new(vec![anchor(2000, 1000), anchor(2005, 1005)]);
        assert_eq!(
            map.wall_to_mach(1_999_000_000),
            Some(999_000_000_000),
            "before the first anchor"
        );
        assert_eq!(
            map.wall_to_mach(2_010_000_000),
            Some(1_010_000_000_000),
            "after the last anchor"
        );
    }

    #[test]
    fn handles_a_clock_step() {
        // the wall clock jumps forward 30s between the second and third anchors (an NTP step, or
        // the machine slept while mach time was paused)
        let map = ClockMap::new(vec![
            anchor(2000, 1000),
            anchor(2005, 1005),
            anchor(2040, 1010),
        ]);

        // before the step the offset is 1000s
        assert_eq!(map.wall_to_mach(2_001_000_000), Some(1_001_000_000_000));
        // after the step the offset is 1030s
        assert_eq!(map.wall_to_mach(2_039_000_000), Some(1_009_000_000_000));
        // and mach -> wall agrees on both sides
        assert_eq!(map.mach_to_wall(1_001_000_000_000), Some(2_001_000_000));
        assert_eq!(map.mach_to_wall(1_009_000_000_000), Some(2_039_000_000));
    }

    #[test]
    fn single_anchor_is_a_constant_offset() {
        let map = ClockMap::new(vec![anchor(2000, 1000)]);
        assert_eq!(map.wall_to_mach(2_003_000_000), Some(1_003_000_000_000));
        assert_eq!(map.mach_to_wall(1_003_000_000_000), Some(2_003_000_000));
    }

    #[test]
    fn the_jump_detector_ignores_ordinary_time_passing() {
        let mut detector = JumpDetector::new(anchor(2000, 1000));
        // 50 ms later on both clocks, with a little drift
        let tick = |wall_us: i64, mach_ns: u64| Anchor { wall_us, mach_ns };
        assert_eq!(
            detector.observe(tick(2_000_050_010, 1_000_050_000_000)),
            None
        );
        // a long but consistent interval (the process was merely descheduled)
        assert_eq!(
            detector.observe(tick(2_030_050_010, 1_030_050_000_000)),
            None
        );
    }

    #[test]
    fn the_jump_detector_notices_sleep_and_brackets_it() {
        let mut detector = JumpDetector::new(anchor(2000, 1000));
        let before = Anchor {
            wall_us: 2_000_050_000,
            mach_ns: 1_000_050_000_000,
        };
        assert_eq!(detector.observe(before), None);

        // the lid is closed for 3600 s: wall time moves on, mach time barely does
        let after = Anchor {
            wall_us: 2_000_100_000 + 3_600_000_000,
            mach_ns: 1_000_100_000_000,
        };
        let jump = detector.observe(after).expect("a jump");
        assert!((jump.seconds - 3_600.0).abs() < 0.01, "{}", jump.seconds);
        assert_eq!(
            jump.before, before,
            "the last good pairing before the sleep"
        );
        assert_eq!(jump.after, after);

        // and with those two anchors the map is right on both sides of the sleep
        let map = ClockMap::new(vec![jump.before, jump.after]);
        assert_eq!(map.wall_to_mach(2_000_050_000), Some(1_000_050_000_000));
        assert_eq!(
            map.wall_to_mach(2_000_100_000 + 3_600_000_000),
            Some(1_000_100_000_000)
        );
        assert_eq!(
            map.wall_to_mach(2_000_100_000 + 3_600_000_000 + 2_000_000),
            Some(1_000_100_000_000 + 2_000_000_000),
            "two seconds after waking"
        );

        // the detector carries on from the new position
        assert_eq!(
            detector.observe(Anchor {
                wall_us: after.wall_us + 50_000,
                mach_ns: after.mach_ns + 50_000_000
            }),
            None
        );
    }

    #[test]
    fn the_jump_detector_notices_the_clock_stepping_back() {
        let mut detector = JumpDetector::new(anchor(2000, 1000));
        let jump = detector
            .observe(Anchor {
                wall_us: 2_000_050_000 - 30_000_000,
                mach_ns: 1_000_050_000_000,
            })
            .expect("a jump");
        assert!(
            jump.seconds < -29.0 && jump.seconds > -31.0,
            "{}",
            jump.seconds
        );
    }

    #[test]
    fn empty_map_has_no_answer() {
        let map = ClockMap::default();
        assert_eq!(map.wall_to_mach(1), None);
        assert_eq!(map.mach_to_wall(1), None);
    }
}
