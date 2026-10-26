//! Parsing and formatting of video timecodes.
//!
//! Accepted forms: `12.5` (seconds), `12` , `00:12.500`, `1:02`, `1:02:03` and `1:02:03.25`.
//! A leading `-` is accepted so that offsets before the start of the video can be expressed.

use color_eyre::eyre;

pub fn parse(input: &str) -> eyre::Result<f64> {
    let trimmed = input.trim();
    let (negative, body) = match trimmed.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, trimmed),
    };

    if body.is_empty() {
        eyre::bail!("empty timecode");
    }

    let parts: Vec<&str> = body.split(':').collect();
    if parts.len() > 3 {
        eyre::bail!("invalid timecode '{input}': expected [[H:]M:]S[.fraction]");
    }

    let mut seconds = 0.0_f64;
    for (index, part) in parts.iter().enumerate() {
        let is_last = index == parts.len() - 1;
        let invalid = || eyre::Report::msg(format!("invalid timecode '{input}'"));
        let value: f64 = if is_last {
            part.parse().map_err(|_| invalid())?
        } else {
            f64::from(part.parse::<u32>().map_err(|_| invalid())?)
        };

        if !value.is_finite() || value < 0.0 {
            eyre::bail!("invalid timecode '{input}'");
        }

        seconds = seconds * 60.0 + value;
    }

    Ok(if negative { -seconds } else { seconds })
}

/// Parse a span such as `1s`, `500ms`, `1.5` (seconds) or `2m`.
pub fn parse_span(input: &str) -> eyre::Result<f64> {
    let trimmed = input.trim();
    let (number, scale) = if let Some(rest) = trimmed.strip_suffix("ms") {
        (rest, 0.001)
    } else if let Some(rest) = trimmed.strip_suffix('s') {
        (rest, 1.0)
    } else if let Some(rest) = trimmed.strip_suffix('m') {
        (rest, 60.0)
    } else {
        (trimmed, 1.0)
    };

    let value: f64 = number.trim().parse().map_err(|_| {
        eyre::Report::msg(format!(
            "invalid duration '{input}': expected e.g. 1s, 500ms or 1.5"
        ))
    })?;

    if !value.is_finite() || value < 0.0 {
        eyre::bail!("invalid duration '{input}': must be a non-negative number");
    }

    Ok(value * scale)
}

#[must_use]
pub fn format(seconds: f64) -> String {
    let sign = if seconds < 0.0 { "-" } else { "" };
    let total_ms = (seconds.abs() * 1_000.0).round() as u64;
    let ms = total_ms % 1_000;
    let total_s = total_ms / 1_000;
    let s = total_s % 60;
    let m = (total_s / 60) % 60;
    let h = total_s / 3_600;

    if h > 0 {
        format!("{sign}{h}:{m:02}:{s:02}.{ms:03}")
    } else {
        format!("{sign}{m:02}:{s:02}.{ms:03}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn parses_plain_seconds() {
        assert!(close(parse("12.5").unwrap(), 12.5));
        assert!(close(parse("12").unwrap(), 12.0));
        assert!(close(parse("0.04").unwrap(), 0.04));
    }

    #[test]
    fn parses_clock_forms() {
        assert!(close(parse("00:12.500").unwrap(), 12.5));
        assert!(close(parse("1:02").unwrap(), 62.0));
        assert!(close(parse("1:02:03").unwrap(), 3723.0));
        assert!(close(parse("1:02:03.25").unwrap(), 3723.25));
    }

    #[test]
    fn parses_negative_offsets() {
        assert!(close(parse("-1.5").unwrap(), -1.5));
        assert!(close(parse("-0:01.500").unwrap(), -1.5));
    }

    #[test]
    fn rejects_garbage() {
        for bad in [
            "", "abc", "1:2:3:4", "1.5:30", "::", "1:-2", "--1", "nan", "inf",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn parses_spans() {
        assert!(close(parse_span("1s").unwrap(), 1.0));
        assert!(close(parse_span("500ms").unwrap(), 0.5));
        assert!(close(parse_span("1.5").unwrap(), 1.5));
        assert!(close(parse_span("2m").unwrap(), 120.0));
        assert!(parse_span("-1s").is_err());
        assert!(parse_span("soon").is_err());
        assert!(parse_span("").is_err());
    }

    #[test]
    fn formats() {
        assert_eq!(format(12.5), "00:12.500");
        assert_eq!(format(0.0), "00:00.000");
        assert_eq!(format(62.0), "01:02.000");
        assert_eq!(format(3723.25), "1:02:03.250");
        assert_eq!(format(-1.5), "-00:01.500");
    }

    #[test]
    fn format_parse_roundtrip() {
        for value in [0.0, 0.001, 12.5, 59.999, 3600.0, 7322.125] {
            assert!(close(parse(&format(value)).unwrap(), value));
        }
    }
}
