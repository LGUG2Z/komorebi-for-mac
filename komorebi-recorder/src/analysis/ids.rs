//! Stable names for things in a session, so a command's output can be fed straight back in.
//!
//! An event is `event:<seq>`, a line of the recorded komorebi log is `log:<line>`, a recorder
//! incident is `incident:<n>` (1-based line of `recorder.jsonl`) and a user or agent annotation is
//! `bug:<id>`, `note:<id>` or `finding:<id>`. All of them are accepted wherever a time is, see
//! [`crate::analysis::timespec`].

use color_eyre::eyre;
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::BuildHasher;
use std::hash::Hasher;
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemRef {
    Event(u64),
    Log(u64),
    Incident(u64),
    Annotation(String),
}

impl fmt::Display for ItemRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Event(seq) => write!(f, "event:{seq}"),
            Self::Log(line) => write!(f, "log:{line}"),
            Self::Incident(number) => write!(f, "incident:{number}"),
            Self::Annotation(id) => write!(f, "ann:{id}"),
        }
    }
}

impl ItemRef {
    /// Whether `input` starts with one of the prefixes [`ItemRef`] understands.
    #[must_use]
    pub fn has_prefix(input: &str) -> bool {
        input
            .split_once(':')
            .is_some_and(|(prefix, _)| PREFIXES.contains(&prefix))
    }
}

const PREFIXES: [&str; 7] = ["event", "log", "incident", "ann", "bug", "note", "finding"];

impl FromStr for ItemRef {
    type Err = eyre::Report;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        // a bare annotation id is fine
        if is_annotation_id(input) {
            return Ok(Self::Annotation(input.to_string()));
        }

        let (prefix, rest) = input
            .split_once(':')
            .ok_or_else(|| super::envelope::usage(format!("'{input}' is not an item reference")))?;
        let number = |what: &str| -> eyre::Result<u64> {
            rest.parse().map_err(|_| {
                super::envelope::usage(format!("'{input}': {what} must be a whole number"))
            })
        };

        match prefix {
            "event" => Ok(Self::Event(number("an event sequence number")?)),
            "log" => Ok(Self::Log(number("a log line")?)),
            "incident" => Ok(Self::Incident(number("an incident number")?)),
            "ann" | "bug" | "note" | "finding" if is_annotation_id(rest) => {
                Ok(Self::Annotation(rest.to_string()))
            }
            "ann" | "bug" | "note" | "finding" => Err(super::envelope::usage(format!(
                "'{rest}' is not an annotation id like a-3f9c1e02"
            ))),
            _ => Err(super::envelope::usage(format!(
                "unknown reference '{input}': expected event:<seq>, log:<line>, incident:<n> or bug:<id>"
            ))),
        }
    }
}

/// `a-` and eight lowercase hex digits.
#[must_use]
pub fn is_annotation_id(text: &str) -> bool {
    text.strip_prefix("a-").is_some_and(|hex| {
        hex.len() == 8
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// A fresh annotation id. Random per process (the standard library seeds its hasher from the OS)
/// and mixed with the clock, so two recorders marking at once do not collide.
#[must_use]
pub fn new_annotation_id() -> String {
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u64(crate::clock::mach_now_ns());
    hasher.write_u32(std::process::id());
    format!("a-{:08x}", hasher.finish() as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_round_trip() {
        for text in ["event:12", "log:345", "incident:2", "ann:a-3f9c1e02"] {
            let item: ItemRef = text.parse().expect(text);
            assert_eq!(item.to_string(), text);
        }
    }

    #[test]
    fn annotation_kinds_and_bare_ids_are_aliases() {
        let expected = ItemRef::Annotation("a-3f9c1e02".to_string());
        for text in [
            "bug:a-3f9c1e02",
            "note:a-3f9c1e02",
            "finding:a-3f9c1e02",
            "a-3f9c1e02",
        ] {
            assert_eq!(text.parse::<ItemRef>().expect(text), expected);
        }
    }

    #[test]
    fn bad_references_are_usage_errors() {
        for text in [
            "event:x",
            "log:",
            "bug:123",
            "frame:3",
            "nonsense",
            "bug:a-XYZ",
        ] {
            let error = text.parse::<ItemRef>().expect_err(text);
            assert_eq!(
                super::super::envelope::exit_kind(&error),
                super::super::envelope::ExitKind::Usage,
                "{text}"
            );
        }
    }

    #[test]
    fn new_ids_look_right_and_do_not_collide() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let id = new_annotation_id();
            assert!(is_annotation_id(&id), "{id}");
            seen.insert(id);
        }
        assert!(seen.len() > 990, "only {} distinct ids", seen.len());
    }
}
