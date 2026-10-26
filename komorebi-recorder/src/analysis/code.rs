//! From a recorded log line to the source that wrote it.
//!
//! This is what turns "komorebi did something odd at 00:12" into "this statement, in this function,
//! ran": each line of a handler's output is matched to the `tracing` statement in komorebi's source
//! that produced it (see [`super::catalog`]), shown with the code around it, and described by the
//! role of the part of komorebi it came from. Every answer carries whether the source is the
//! version that was recorded.

use super::Analysis;
use super::catalog;
use super::catalog::Skew;
use super::catalog::Source;
use super::catalog::Statement;
use super::envelope;
use color_eyre::eyre;
use schemars::JsonSchema;
use serde::Serialize;
use std::path::Path;

pub const SCHEMA_CODE: &str = "code/1";

/// A few lines of komorebi's source around a statement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Snippet {
    pub file: String,
    /// The statement's line.
    pub line: usize,
    /// `(line number, text)` of each line shown.
    pub lines: Vec<(usize, String)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct LineSource {
    /// The line of the session's `komorebi.log`.
    pub line: u64,
    pub video_s: Option<f64>,
    pub level: Option<String>,
    pub target: Option<String>,
    pub message: String,
    /// The part of komorebi that logged it, in a sentence.
    pub role: Option<String>,
    /// That part changes state without sending a notification.
    pub changes_state_silently: bool,
    /// The statement that wrote it, if one matched.
    pub source: Option<Source>,
    pub snippet: Option<Snippet>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct CodeData {
    pub skew: Skew,
    /// What was asked about, e.g. `event:12` or `log:345`.
    pub about: String,
    pub lines: Vec<LineSource>,
    /// Statements whose text contains what was searched for (`--grep`).
    pub statements: Vec<Statement>,
}

/// Where to read komorebi's source from.
fn snippet(root: Option<&Path>, statement: &Statement, context: usize) -> Option<Snippet> {
    let text = std::fs::read_to_string(root?.join(&statement.file)).ok()?;
    let all: Vec<&str> = text.lines().collect();
    let first = statement.line.saturating_sub(context).max(1);
    let last = (statement.line + context).min(all.len());
    (first <= last).then(|| Snippet {
        file: statement.file.clone(),
        line: statement.line,
        lines: (first..=last)
            .map(|number| (number, all[number - 1].to_string()))
            .collect(),
    })
}

impl Analysis {
    /// The source of each of the given lines of komorebi's log.
    pub fn code_for_lines(
        &self,
        about: &str,
        numbers: &[u64],
        context: usize,
        root: Option<&Path>,
    ) -> eyre::Result<CodeData> {
        let log = self.log();
        let mut lines = Vec::new();
        for number in numbers {
            let line = usize::try_from(number.saturating_sub(1))
                .ok()
                .filter(|_| *number >= 1)
                .and_then(|index| log.lines.get(index))
                .ok_or_else(|| {
                    envelope::not_found(format!(
                        "komorebi.log has {} lines; there is no line {number}",
                        log.lines.len()
                    ))
                })?;

            let source = match (&line.target, line.level) {
                (Some(target), Some(level)) => catalog::find(target, level, &line.message),
                _ => None,
            };
            let role = line.target.as_deref().map(catalog::role);

            lines.push(LineSource {
                line: line.number,
                video_s: line
                    .wall_us
                    .and_then(|wall_us| self.clock().wall_to_mach(wall_us))
                    .map(|mach_ns| self.video_s(mach_ns)),
                level: line.level.map(|level| format!("{level:?}").to_uppercase()),
                target: line.target.clone(),
                message: line.message.clone(),
                role: role.map(|role| role.summary.to_string()),
                changes_state_silently: role.is_some_and(|role| role.changes_state_silently),
                snippet: source
                    .as_ref()
                    .and_then(|source| snippet(root, &source.statement, context)),
                source,
            });
        }

        Ok(CodeData {
            skew: catalog::skew(&self.manifest.komorebi.versions),
            about: about.to_string(),
            lines,
            statements: Vec::new(),
        })
    }

    /// The source of everything the handler of event `seq` logged.
    pub fn code_for_event(
        &self,
        seq: u64,
        context: usize,
        root: Option<&Path>,
    ) -> eyre::Result<CodeData> {
        let index = self
            .event_index(seq)
            .ok_or_else(|| envelope::not_found(format!("there is no event #{seq}")))?;
        let block = self
            .pairing()
            .pairs
            .get(index)
            .and_then(|pair| pair.block)
            .and_then(|id| self.log().blocks.get(id))
            .ok_or_else(|| {
                envelope::not_found(format!(
                    "no handler lines were found in komorebi's log for event #{seq}: the log may \
                     not have been recorded, or komorebi's log level may not show its handler"
                ))
            })?;

        self.code_for_lines(&format!("event:{seq}"), &block.lines.clone(), context, root)
    }

    /// Statements whose text contains `text`, without needing a log line.
    #[must_use]
    pub fn code_search(&self, text: &str) -> CodeData {
        let needle = text.to_lowercase();
        CodeData {
            skew: catalog::skew(&self.manifest.komorebi.versions),
            about: format!("grep {text}"),
            lines: Vec::new(),
            statements: catalog::statements_matching(&needle),
        }
    }
}
