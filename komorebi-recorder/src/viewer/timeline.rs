//! The arithmetic of the timeline: which stretch of the recording is visible, where a time falls
//! on the screen, where the ruler's marks go, and how to squeeze thousands of events into a few
//! hundred pixels. No drawing happens here, so all of it is tested.

/// The stretch of the recording that is on screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub start_s: f64,
    pub end_s: f64,
}

/// The shortest stretch it will zoom in to.
pub const MIN_SPAN_S: f64 = 0.05;

impl View {
    /// All of a recording of `duration_s`.
    #[must_use]
    pub fn whole(duration_s: f64) -> Self {
        Self {
            start_s: 0.0,
            end_s: duration_s.max(MIN_SPAN_S),
        }
    }

    #[must_use]
    pub fn span(&self) -> f64 {
        self.end_s - self.start_s
    }

    /// Where `time` falls across `width` pixels starting at `left`.
    #[must_use]
    pub fn x(&self, time: f64, left: f32, width: f32) -> f32 {
        left + ((time - self.start_s) / self.span()) as f32 * width
    }

    /// The time at pixel `x`.
    #[must_use]
    pub fn time(&self, x: f32, left: f32, width: f32) -> f64 {
        self.start_s + f64::from((x - left) / width.max(1.0)) * self.span()
    }

    /// Zoom by `factor` (above 1 zooms in) keeping the time under `anchor_s` where it is on the
    /// screen, without leaving `0..=duration_s`.
    pub fn zoom(&mut self, anchor_s: f64, factor: f64, duration_s: f64) {
        let factor = factor.clamp(0.01, 100.0);
        let span = (self.span() / factor).clamp(MIN_SPAN_S, duration_s.max(MIN_SPAN_S));
        // the anchor keeps its place in the window
        let share = ((anchor_s - self.start_s) / self.span()).clamp(0.0, 1.0);
        self.start_s = anchor_s - share * span;
        self.end_s = self.start_s + span;
        self.clamp(duration_s);
    }

    /// Move by `delta_s`, stopping at the ends of the recording.
    pub fn pan(&mut self, delta_s: f64, duration_s: f64) {
        self.start_s += delta_s;
        self.end_s += delta_s;
        self.clamp(duration_s);
    }

    /// Make sure `time` is on screen, moving the window the least that does it.
    pub fn reveal(&mut self, time: f64, duration_s: f64) {
        let margin = self.span() * 0.1;
        if time < self.start_s + margin {
            self.pan(time - margin - self.start_s, duration_s);
        } else if time > self.end_s - margin {
            self.pan(time + margin - self.end_s, duration_s);
        }
    }

    /// Centre on `time`, keeping the zoom.
    pub fn centre(&mut self, time: f64, duration_s: f64) {
        let half = self.span() / 2.0;
        self.start_s = time - half;
        self.end_s = time + half;
        self.clamp(duration_s);
    }

    fn clamp(&mut self, duration_s: f64) {
        let duration = duration_s.max(MIN_SPAN_S);
        let span = self.span().min(duration);
        if self.start_s < 0.0 {
            self.start_s = 0.0;
        }
        self.end_s = self.start_s + span;
        if self.end_s > duration {
            self.end_s = duration;
            self.start_s = duration - span;
        }
    }
}

/// A mark on the ruler.
#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    pub time: f64,
    /// Labelled with a time.
    pub major: bool,
    pub label: String,
}

/// The steps a ruler counts in, in seconds.
const STEPS: [f64; 15] = [
    0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0, 1800.0,
];

fn label(time: f64, step: f64) -> String {
    let total = time.max(0.0);
    let minutes = (total / 60.0) as u64;
    let seconds = total - (minutes * 60) as f64;
    if step >= 1.0 {
        format!("{minutes}:{:02}", seconds.round() as u64 % 60)
    } else if step >= 0.1 {
        format!("{minutes}:{seconds:04.1}")
    } else {
        format!("{minutes}:{seconds:05.2}")
    }
}

/// The marks of a ruler across `width` pixels: a labelled one at least every `min_label_px`, and
/// minor ones between them.
#[must_use]
pub fn ruler(view: &View, width: f32, min_label_px: f32) -> Vec<Tick> {
    if width <= 0.0 || view.span() <= 0.0 {
        return Vec::new();
    }

    let seconds_per_px = view.span() / f64::from(width);
    let step = STEPS
        .iter()
        .copied()
        .find(|step| step / seconds_per_px >= f64::from(min_label_px))
        .unwrap_or(3600.0);
    let minor = step / 5.0;

    let mut ticks = Vec::new();
    let first = (view.start_s / minor).floor() as i64;
    let last = (view.end_s / minor).ceil() as i64;
    for index in first..=last {
        let time = index as f64 * minor;
        if time < view.start_s - 1e-9 || time > view.end_s + 1e-9 {
            continue;
        }
        let major = index % 5 == 0;
        ticks.push(Tick {
            time,
            major,
            label: if major {
                label(time, step)
            } else {
                String::new()
            },
        });
    }

    ticks
}

/// A column of pixels in a dense lane: how many things fall in it and the most important kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bin {
    pub column: usize,
    pub count: usize,
    /// The largest weight of anything in the column.
    pub weight: u8,
}

/// Count items into one-pixel columns of `width` across the view. `items` yields `(time, weight)`.
#[must_use]
pub fn bin(items: impl Iterator<Item = (f64, u8)>, view: &View, width: usize) -> Vec<Bin> {
    if width == 0 {
        return Vec::new();
    }

    let mut columns: Vec<Option<Bin>> = vec![None; width];
    for (time, weight) in items {
        if time < view.start_s || time > view.end_s {
            continue;
        }

        let column =
            (((time - view.start_s) / view.span()) * width as f64).min(width as f64 - 1.0) as usize;
        let slot = columns[column].get_or_insert(Bin {
            column,
            count: 0,
            weight: 0,
        });
        slot.count += 1;
        slot.weight = slot.weight.max(weight);
    }

    columns.into_iter().flatten().collect()
}

/// The index of the item nearest `time` among sorted `times`, if it is within `reach_s`.
#[must_use]
pub fn nearest(times: &[f64], time: f64, reach_s: f64) -> Option<usize> {
    if times.is_empty() {
        return None;
    }

    let after = times.partition_point(|candidate| *candidate < time);
    let candidates = [after.checked_sub(1), (after < times.len()).then_some(after)];
    candidates
        .into_iter()
        .flatten()
        .min_by(|a, b| {
            (times[*a] - time)
                .abs()
                .total_cmp(&(times[*b] - time).abs())
        })
        .filter(|index| (times[*index] - time).abs() <= reach_s)
}

/// The first of sorted `times` strictly after `time`.
#[must_use]
pub fn next_after(times: &[f64], time: f64) -> Option<f64> {
    let index = times.partition_point(|candidate| *candidate <= time + 1e-6);
    times.get(index).copied()
}

/// The last of sorted `times` strictly before `time`.
#[must_use]
pub fn previous_before(times: &[f64], time: f64) -> Option<f64> {
    let index = times.partition_point(|candidate| *candidate < time - 1e-6);
    index
        .checked_sub(1)
        .and_then(|index| times.get(index).copied())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn times_and_pixels_convert_both_ways() {
        let view = View {
            start_s: 10.0,
            end_s: 20.0,
        };
        assert!((view.x(15.0, 100.0, 500.0) - 350.0).abs() < 1e-3);
        assert!(close(view.time(350.0, 100.0, 500.0), 15.0));
        for time in [10.0, 12.5, 19.9] {
            let x = view.x(time, 40.0, 800.0);
            assert!(close(view.time(x, 40.0, 800.0), time));
        }
    }

    #[test]
    fn zooming_keeps_the_time_under_the_cursor_where_it_is() {
        let mut view = View::whole(100.0);
        view.zoom(30.0, 4.0, 100.0);

        assert!(close(view.span(), 25.0));
        // 30 s was 30% of the way across and still is
        assert!(close((30.0 - view.start_s) / view.span(), 0.3));

        view.zoom(30.0, 0.25, 100.0);
        assert!(close(view.span(), 100.0));
        assert!(close(view.start_s, 0.0));
    }

    #[test]
    fn zooming_stops_at_the_ends_and_at_the_closest_zoom() {
        let mut view = View::whole(10.0);
        view.zoom(0.0, 0.1, 10.0);
        assert!(close(view.span(), 10.0), "cannot show more than there is");

        for _ in 0..50 {
            view.zoom(5.0, 2.0, 10.0);
        }
        assert!(close(view.span(), MIN_SPAN_S));
        assert!(view.start_s >= 0.0 && view.end_s <= 10.0);
    }

    #[test]
    fn panning_stops_at_the_ends() {
        let mut view = View {
            start_s: 20.0,
            end_s: 30.0,
        };
        view.pan(-50.0, 100.0);
        assert!(close(view.start_s, 0.0) && close(view.end_s, 10.0));

        view.pan(500.0, 100.0);
        assert!(close(view.start_s, 90.0) && close(view.end_s, 100.0));
    }

    #[test]
    fn revealing_moves_the_window_only_when_the_time_is_off_screen() {
        let mut view = View {
            start_s: 20.0,
            end_s: 30.0,
        };
        view.reveal(25.0, 100.0);
        assert!(close(view.start_s, 20.0), "already visible: unmoved");

        view.reveal(40.0, 100.0);
        assert!(view.end_s >= 40.0 && view.start_s > 20.0);
        assert!(close(view.span(), 10.0), "the zoom is kept");

        view.reveal(2.0, 100.0);
        assert!(view.start_s <= 2.0);
    }

    #[test]
    fn centring_keeps_the_zoom_and_the_ends() {
        let mut view = View {
            start_s: 0.0,
            end_s: 10.0,
        };
        view.centre(50.0, 100.0);
        assert!(close(view.start_s, 45.0) && close(view.end_s, 55.0));
        view.centre(2.0, 100.0);
        assert!(close(view.start_s, 0.0) && close(view.end_s, 10.0));
    }

    #[test]
    fn the_ruler_counts_in_round_steps_that_are_never_too_close() {
        let ticks = ruler(&View::whole(60.0), 1200.0, 80.0);
        let majors: Vec<&Tick> = ticks.iter().filter(|tick| tick.major).collect();

        // 60 s over 1200 px is 0.05 s per pixel: 80 px is 4 s, so labels every 5 s
        assert!(close(majors[1].time - majors[0].time, 5.0), "{majors:?}");
        for pair in majors.windows(2) {
            let pixels = (pair[1].time - pair[0].time) / (60.0 / 1200.0);
            assert!(pixels >= 80.0 - 1e-6, "labels {pixels} px apart");
        }
        assert_eq!(majors[0].label, "0:00");
        assert_eq!(majors[1].label, "0:05");
        assert_eq!(majors[2].label, "0:10");
        // minor marks sit between
        assert!(ticks.iter().any(|tick| !tick.major));
    }

    #[test]
    fn zoomed_in_the_ruler_labels_fractions_of_a_second() {
        let view = View {
            start_s: 12.0,
            end_s: 12.5,
        };
        let ticks = ruler(&view, 1000.0, 80.0);
        let labels: Vec<&str> = ticks
            .iter()
            .filter(|tick| tick.major)
            .map(|tick| tick.label.as_str())
            .collect();
        // 0.5 s over 1000 px with labels 80 px apart: a label every 0.05 s, to two places
        assert_eq!(labels.len(), 11, "{labels:?}");
        assert_eq!(labels[0], "0:12.00");
        assert_eq!(labels[2], "0:12.10");
        assert_eq!(labels[10], "0:12.50");
        assert!(
            ticks
                .iter()
                .all(|tick| tick.time >= 12.0 - 1e-9 && tick.time <= 12.5 + 1e-9)
        );
    }

    #[test]
    fn a_degenerate_ruler_is_empty_not_a_panic() {
        assert!(ruler(&View::whole(10.0), 0.0, 80.0).is_empty());
        assert!(
            ruler(
                &View {
                    start_s: 5.0,
                    end_s: 5.0
                },
                100.0,
                80.0
            )
            .is_empty()
        );
    }

    #[test]
    fn many_items_become_columns_counting_what_fell_in_them() {
        let view = View {
            start_s: 0.0,
            end_s: 10.0,
        };
        let items = vec![
            (0.05, 1),
            (0.06, 3),
            (0.07, 2),
            (5.0, 1),
            (9.99, 2),
            (11.0, 9),
            (-1.0, 9),
        ];
        let columns = bin(items.into_iter(), &view, 10);

        assert_eq!(columns.len(), 3, "{columns:?}");
        assert_eq!(
            columns[0],
            Bin {
                column: 0,
                count: 3,
                weight: 3
            }
        );
        assert_eq!(
            columns[1],
            Bin {
                column: 5,
                count: 1,
                weight: 1
            }
        );
        // the last instant belongs to the last column, and things off screen to none
        assert_eq!(
            columns[2],
            Bin {
                column: 9,
                count: 1,
                weight: 2
            }
        );
    }

    #[test]
    fn the_nearest_item_must_be_within_reach() {
        let times = [1.0, 2.0, 4.0, 8.0];
        assert_eq!(nearest(&times, 2.2, 0.5), Some(1));
        assert_eq!(nearest(&times, 3.1, 1.5), Some(2));
        assert_eq!(nearest(&times, 5.0, 0.5), None);
        assert_eq!(nearest(&times, 0.9, 0.5), Some(0));
        assert_eq!(nearest(&times, 9.0, 2.0), Some(3));
        assert_eq!(nearest(&[], 1.0, 5.0), None);
    }

    #[test]
    fn next_and_previous_skip_the_one_you_are_on() {
        let times = [1.0, 2.0, 4.0];
        assert_eq!(next_after(&times, 2.0), Some(4.0));
        assert_eq!(next_after(&times, 1.5), Some(2.0));
        assert_eq!(next_after(&times, 4.0), None);
        assert_eq!(previous_before(&times, 2.0), Some(1.0));
        assert_eq!(previous_before(&times, 2.5), Some(2.0));
        assert_eq!(previous_before(&times, 1.0), None);
    }
}
