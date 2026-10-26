//! The shape every investigation command prints with `--json`, and how a failure maps to an exit
//! code.
//!
//! An agent driving the CLI should never have to guess whether a command worked, whether the output
//! was cut short, or why it failed, so every command answers with the same envelope and exits with
//! a code that says what kind of failure it was.

use color_eyre::eyre;
use schemars::JsonSchema;
use serde::Serialize;
use std::fmt;

/// Bumped when the envelope's own fields change (each command's `data` is versioned by `schema`).
pub const ENVELOPE_VERSION: u32 = 1;

/// How the process exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ExitKind {
    Ok,
    /// Something went wrong that is not one of the cases below.
    Error,
    /// The command line was wrong.
    Usage,
    /// The session, or something asked for in it, does not exist.
    NotFound,
    /// The session exists but cannot answer: still recording, or it recorded nothing.
    Unusable,
    /// The answer would have exceeded the time or size budget.
    Budget,
}

impl ExitKind {
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Error => 1,
            Self::Usage => 2,
            Self::NotFound => 3,
            Self::Unusable => 4,
            Self::Budget => 5,
        }
    }
}

/// An error that knows how the process should exit and, optionally, what to do about it.
#[derive(Debug)]
pub struct CliError {
    pub kind: ExitKind,
    pub message: String,
    pub hint: Option<String>,
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, " ({hint})")?;
        }
        Ok(())
    }
}

impl std::error::Error for CliError {}

fn failure(kind: ExitKind, message: impl Into<String>) -> eyre::Report {
    eyre::Report::new(CliError {
        kind,
        message: message.into(),
        hint: None,
    })
}

pub fn usage(message: impl Into<String>) -> eyre::Report {
    failure(ExitKind::Usage, message)
}

pub fn not_found(message: impl Into<String>) -> eyre::Report {
    failure(ExitKind::NotFound, message)
}

pub fn unusable(message: impl Into<String>) -> eyre::Report {
    failure(ExitKind::Unusable, message)
}

pub fn over_budget(message: impl Into<String>) -> eyre::Report {
    failure(ExitKind::Budget, message)
}

/// The same failure with a pointer to what the caller can do next.
pub fn with_hint(report: eyre::Report, hint: impl Into<String>) -> eyre::Report {
    match report.downcast::<CliError>() {
        Ok(mut error) => {
            error.hint = Some(hint.into());
            eyre::Report::new(error)
        }
        Err(report) => report.wrap_err(hint.into()),
    }
}

/// How `report` should make the process exit.
#[must_use]
pub fn exit_kind(report: &eyre::Report) -> ExitKind {
    report
        .chain()
        .find_map(|cause| cause.downcast_ref::<CliError>())
        .map_or(ExitKind::Error, |error| error.kind)
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ErrorBody {
    pub kind: ExitKind,
    pub code: u8,
    pub message: String,
    pub hint: Option<String>,
}

/// What a command prints with `--json`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Envelope<T> {
    pub ok: bool,
    /// Which command's `data` this is, e.g. `brief/1`; see `komorebi-recorder schema`.
    pub schema: String,
    pub envelope_version: u32,
    pub data: Option<T>,
    pub warnings: Vec<String>,
    /// The output was cut short; pass `next_cursor` back as `--cursor` for the rest.
    pub truncated: bool,
    pub next_cursor: Option<String>,
    pub error: Option<ErrorBody>,
}

impl<T> Envelope<T> {
    #[must_use]
    pub fn ok(schema: &str, data: T) -> Self {
        Self {
            ok: true,
            schema: schema.to_string(),
            envelope_version: ENVELOPE_VERSION,
            data: Some(data),
            warnings: Vec::new(),
            truncated: false,
            next_cursor: None,
            error: None,
        }
    }

    #[must_use]
    pub fn warnings(mut self, warnings: Vec<String>) -> Self {
        self.warnings = warnings;
        self
    }

    /// Mark the output as cut short, with how to ask for the rest.
    #[must_use]
    pub fn truncated(mut self, next_cursor: Option<String>) -> Self {
        self.truncated = true;
        self.next_cursor = next_cursor;
        self
    }
}

impl Envelope<()> {
    #[must_use]
    pub fn failed(report: &eyre::Report) -> Self {
        let kind = exit_kind(report);
        let hint = report
            .chain()
            .find_map(|cause| cause.downcast_ref::<CliError>())
            .and_then(|error| error.hint.clone());

        Self {
            ok: false,
            schema: "error/1".to_string(),
            envelope_version: ENVELOPE_VERSION,
            data: None,
            warnings: Vec::new(),
            truncated: false,
            next_cursor: None,
            error: Some(ErrorBody {
                kind,
                code: kind.code(),
                message: report
                    .chain()
                    .next()
                    .map_or_else(String::new, ToString::to_string),
                hint,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_stable() {
        let codes: Vec<u8> = [
            ExitKind::Ok,
            ExitKind::Error,
            ExitKind::Usage,
            ExitKind::NotFound,
            ExitKind::Unusable,
            ExitKind::Budget,
        ]
        .iter()
        .map(|kind| kind.code())
        .collect();
        assert_eq!(codes, [0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn a_plain_error_exits_with_the_generic_code() {
        assert_eq!(exit_kind(&eyre::Report::msg("boom")), ExitKind::Error);
    }

    #[test]
    fn the_kind_survives_added_context() {
        use color_eyre::eyre::WrapErr;

        let report = Err::<(), _>(not_found("no such event"))
            .wrap_err("while resolving the start")
            .unwrap_err();
        assert_eq!(exit_kind(&report), ExitKind::NotFound);
    }

    #[test]
    fn a_hint_travels_with_the_error() {
        let report = with_hint(unusable("still recording"), "run `reindex` if it died");
        assert_eq!(exit_kind(&report), ExitKind::Unusable);

        let envelope = Envelope::failed(&report);
        let body = envelope.error.expect("error body");
        assert_eq!(body.code, 4);
        assert_eq!(body.hint.as_deref(), Some("run `reindex` if it died"));
        assert!(!envelope.ok);
    }

    #[test]
    fn the_envelope_has_a_fixed_shape() {
        let envelope = Envelope::ok("demo/1", vec![1, 2, 3])
            .warnings(vec!["careful".to_string()])
            .truncated(Some("c2".to_string()));
        let value = serde_json::to_value(&envelope).expect("serialize");

        assert_eq!(value["ok"], true);
        assert_eq!(value["schema"], "demo/1");
        assert_eq!(value["envelope_version"], ENVELOPE_VERSION);
        assert_eq!(value["data"], serde_json::json!([1, 2, 3]));
        assert_eq!(value["warnings"], serde_json::json!(["careful"]));
        assert_eq!(value["truncated"], true);
        assert_eq!(value["next_cursor"], "c2");
        assert!(value["error"].is_null());
    }
}
