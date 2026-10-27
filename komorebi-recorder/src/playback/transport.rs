//! The playhead: where in the recording it is, and whether and how fast it is moving.
//!
//! Position is computed from a starting point, a rate and the time that has passed, never
//! accumulated frame by frame, so it does not drift whatever the frame rate of the video or of the
//! window drawing it. Times are session video seconds; `now` is any monotonic clock in seconds.

/// How fast it may play, forwards or backwards.
pub const MAX_RATE: f64 = 8.0;
pub const MIN_RATE: f64 = 0.1;

#[derive(Debug, Clone, PartialEq)]
pub struct Transport {
    duration_s: f64,
    /// Where it was at `anchored_at`.
    anchor_s: f64,
    anchored_at: f64,
    playing: bool,
    /// Speed as a multiple of real time; negative plays backwards.
    rate: f64,
    looping: Option<(f64, f64)>,
}

impl Transport {
    #[must_use]
    pub fn new(duration_s: f64) -> Self {
        Self {
            duration_s: duration_s.max(0.0),
            anchor_s: 0.0,
            anchored_at: 0.0,
            playing: false,
            rate: 1.0,
            looping: None,
        }
    }

    #[must_use]
    pub fn duration_s(&self) -> f64 {
        self.duration_s
    }

    /// The speed it plays at when playing: negative for backwards.
    #[must_use]
    pub fn rate(&self) -> f64 {
        self.rate
    }

    #[must_use]
    pub fn looping(&self) -> Option<(f64, f64)> {
        self.looping
    }

    /// Where the playhead is at `now`.
    #[must_use]
    pub fn position(&self, now: f64) -> f64 {
        if !self.playing {
            return self.anchor_s;
        }

        let raw = self.anchor_s + self.rate * (now - self.anchored_at);
        if let Some((from, to)) = self.looping
            && to > from
        {
            if self.rate > 0.0 && raw >= to && self.anchor_s <= to {
                return from + (raw - from).rem_euclid(to - from);
            }
            if self.rate < 0.0 && raw <= from && self.anchor_s >= from {
                return to - (to - raw).rem_euclid(to - from);
            }
        }

        raw.clamp(0.0, self.duration_s)
    }

    /// Whether it is moving at `now`: playing, and not stopped at an end.
    #[must_use]
    pub fn is_playing(&self, now: f64) -> bool {
        if !self.playing {
            return false;
        }
        if self.looping.is_some() {
            return true;
        }

        let raw = self.anchor_s + self.rate * (now - self.anchored_at);
        !(self.rate > 0.0 && raw >= self.duration_s || self.rate < 0.0 && raw <= 0.0)
    }

    /// Settle: if it ran into an end it stops there.
    fn settle(&mut self, now: f64) {
        let here = self.position(now);
        let moving = self.is_playing(now);
        self.anchor_s = here;
        self.anchored_at = now;
        self.playing = moving;
    }

    /// Jump to `time` (clamped to the recording), keeping on playing or staying paused.
    pub fn seek(&mut self, time: f64, now: f64) {
        self.anchor_s = time.clamp(0.0, self.duration_s);
        self.anchored_at = now;
    }

    /// Play at `rate` (negative for backwards) from where it is.
    pub fn play(&mut self, rate: f64, now: f64) {
        self.settle(now);
        let magnitude = rate.abs().clamp(MIN_RATE, MAX_RATE);
        self.rate = if rate < 0.0 { -magnitude } else { magnitude };

        // playing forwards from the end (or backwards from the start) starts over
        if self.rate > 0.0 && self.anchor_s >= self.duration_s {
            self.anchor_s = 0.0;
        } else if self.rate < 0.0 && self.anchor_s <= 0.0 {
            self.anchor_s = self.duration_s;
        }
        self.anchored_at = now;
        self.playing = true;
    }

    pub fn pause(&mut self, now: f64) {
        self.settle(now);
        self.playing = false;
    }

    /// Play if paused, pause if playing.
    pub fn toggle(&mut self, now: f64) {
        if self.is_playing(now) {
            self.pause(now);
        } else {
            let rate = self.rate;
            self.play(rate, now);
        }
    }

    /// Change the speed without changing where it is or whether it is playing.
    pub fn set_rate(&mut self, rate: f64, now: f64) {
        let was_playing = self.is_playing(now);
        self.settle(now);
        let magnitude = rate.abs().clamp(MIN_RATE, MAX_RATE);
        self.rate = if rate < 0.0 { -magnitude } else { magnitude };
        self.playing = was_playing;
    }

    /// Repeat `from..to` while playing; `None` stops repeating.
    pub fn set_loop(&mut self, region: Option<(f64, f64)>, now: f64) {
        self.settle(now);
        self.looping = region
            .map(|(from, to)| {
                (
                    from.clamp(0.0, self.duration_s),
                    to.clamp(0.0, self.duration_s),
                )
            })
            .filter(|(from, to)| to > from);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn a_new_transport_is_paused_at_the_start() {
        let transport = Transport::new(10.0);
        assert!(close(transport.position(0.0), 0.0));
        assert!(close(transport.position(100.0), 0.0));
        assert!(!transport.is_playing(100.0));
    }

    #[test]
    fn playing_advances_with_the_clock_not_with_frames() {
        let mut transport = Transport::new(10.0);
        transport.seek(2.0, 50.0);
        transport.play(1.0, 50.0);

        assert!(close(transport.position(50.0), 2.0));
        assert!(close(transport.position(50.5), 2.5));
        assert!(close(transport.position(53.25), 5.25));
        assert!(transport.is_playing(53.25));
    }

    #[test]
    fn the_speed_scales_the_movement_and_can_run_backwards() {
        let mut transport = Transport::new(10.0);
        transport.seek(5.0, 0.0);
        transport.play(2.0, 0.0);
        assert!(close(transport.position(1.0), 7.0));

        transport.play(-0.5, 1.0);
        assert!(close(transport.position(1.0), 7.0));
        assert!(close(transport.position(3.0), 6.0));
        assert!(transport.rate() < 0.0);
    }

    #[test]
    fn pausing_freezes_where_it_was() {
        let mut transport = Transport::new(10.0);
        transport.play(1.0, 0.0);
        transport.pause(3.0);

        assert!(close(transport.position(3.0), 3.0));
        assert!(close(transport.position(99.0), 3.0));
        assert!(!transport.is_playing(99.0));

        // resuming carries on from there at the same speed
        transport.toggle(100.0);
        assert!(close(transport.position(101.0), 4.0));
    }

    #[test]
    fn it_stops_at_either_end() {
        let mut transport = Transport::new(10.0);
        transport.seek(8.0, 0.0);
        transport.play(1.0, 0.0);
        assert!(close(transport.position(5.0), 10.0));
        assert!(!transport.is_playing(5.0));

        transport.seek(1.0, 6.0);
        transport.play(-1.0, 6.0);
        assert!(close(transport.position(9.0), 0.0));
        assert!(!transport.is_playing(9.0));
    }

    #[test]
    fn playing_from_the_end_starts_over() {
        let mut transport = Transport::new(10.0);
        transport.seek(10.0, 0.0);
        transport.play(1.0, 0.0);
        assert!(close(transport.position(1.0), 1.0));

        transport.seek(0.0, 2.0);
        transport.play(-1.0, 2.0);
        assert!(close(transport.position(3.0), 9.0));
    }

    #[test]
    fn seeking_while_playing_keeps_playing_from_the_new_place() {
        let mut transport = Transport::new(10.0);
        transport.play(1.0, 0.0);
        transport.seek(6.0, 2.0);

        assert!(transport.is_playing(2.0));
        assert!(close(transport.position(2.0), 6.0));
        assert!(close(transport.position(3.0), 7.0));

        // and out of range is clamped, not an error
        transport.seek(99.0, 3.0);
        assert!(close(transport.position(3.0), 10.0));
        transport.seek(-5.0, 3.0);
        assert!(close(transport.position(3.0), 0.0));
    }

    #[test]
    fn a_region_repeats() {
        let mut transport = Transport::new(10.0);
        transport.set_loop(Some((2.0, 4.0)), 0.0);
        transport.seek(3.0, 0.0);
        transport.play(1.0, 0.0);

        assert!(close(transport.position(0.5), 3.5));
        assert!(close(transport.position(1.5), 2.5), "wrapped past 4.0");
        assert!(close(transport.position(100.0), 3.0));
        assert!(transport.is_playing(100.0), "a loop never ends");

        // backwards wraps the other way
        transport.play(-1.0, 100.0);
        assert!(close(transport.position(100.5), 2.5));
        assert!(close(transport.position(101.5), 3.5), "wrapped below 2.0");
    }

    #[test]
    fn playing_into_a_region_from_before_it_does_not_wrap_early() {
        let mut transport = Transport::new(10.0);
        transport.set_loop(Some((4.0, 6.0)), 0.0);
        transport.seek(1.0, 0.0);
        transport.play(1.0, 0.0);

        assert!(close(transport.position(2.0), 3.0));
        assert!(close(transport.position(4.0), 5.0));
        assert!(
            close(transport.position(5.5), 4.5),
            "wrapped once it passed the end of the region"
        );
    }

    #[test]
    fn a_degenerate_region_is_no_region() {
        let mut transport = Transport::new(10.0);
        transport.set_loop(Some((5.0, 5.0)), 0.0);
        assert_eq!(transport.looping(), None);
        transport.set_loop(Some((6.0, 3.0)), 0.0);
        assert_eq!(transport.looping(), None);
        transport.set_loop(Some((2.0, 99.0)), 0.0);
        assert_eq!(transport.looping(), Some((2.0, 10.0)));
    }

    #[test]
    fn changing_the_speed_does_not_move_the_playhead_or_start_it() {
        let mut transport = Transport::new(10.0);
        transport.play(1.0, 0.0);
        transport.set_rate(4.0, 2.0);
        assert!(close(transport.position(2.0), 2.0));
        assert!(close(transport.position(3.0), 6.0));

        transport.pause(3.0);
        transport.set_rate(0.5, 4.0);
        assert!(!transport.is_playing(5.0));
        assert!(close(transport.position(5.0), 6.0));
        assert!(close(transport.rate(), 0.5));

        // out-of-range speeds are held to the limits
        transport.set_rate(1000.0, 5.0);
        assert!(close(transport.rate(), MAX_RATE));
        transport.set_rate(0.0, 5.0);
        assert!(close(transport.rate(), MIN_RATE));
    }
}
