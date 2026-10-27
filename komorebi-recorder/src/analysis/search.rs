//! Finding a word across everything the session recorded.
//!
//! A user says "it happened when Slack woke up" or an error names a window; this looks for the text
//! in komorebi's log, the events, what happened to the machine, the annotations and, on request,
//! the macOS trace, and returns where each hit is so it can be fed straight to `trace`, `code` or
//! `at`.

use super::Analysis;
use super::budget::Budget;
use super::envelope;
use crate::annotations;
use crate::timeline;
use crate::timeline::Filter;
use color_eyre::eyre;
use regex::Regex;
use regex::RegexBuilder;
use schemars::JsonSchema;
use serde::Serialize;

pub const SCHEMA_SEARCH: &str = "search/1";

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Hit {
    /// `komorebi-log`, `events`, `recorder`, `annotations` or `oslog:...`.
    pub source: String,
    pub video_s: f64,
    pub mach_ns: u64,
    /// Something to give another command: `log:345`, `event:12`, `incident:2`, an annotation id
    /// or `oslog:<file>:<line>`.
    pub reference: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct SearchData {
    pub query: String,
    pub hits: Vec<Hit>,
    /// How many hits there were after the cursor, including any not returned.
    pub matching: usize,
    /// Where it looked.
    pub searched: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub pattern: String,
    pub regex: bool,
    pub case_sensitive: bool,
    pub from_mach_ns: Option<u64>,
    pub to_mach_ns: Option<u64>,
    /// Also search the macOS trace (large).
    pub oslog: bool,
    /// Hits already returned.
    pub skip: usize,
    pub limit: Option<usize>,
}

enum Matcher {
    Literal {
        needle: String,
        case_sensitive: bool,
    },
    Pattern(Regex),
}

impl Matcher {
    fn new(query: &SearchQuery) -> eyre::Result<Self> {
        if query.pattern.is_empty() {
            return Err(envelope::usage("nothing to search for"));
        }

        if query.regex {
            return RegexBuilder::new(&query.pattern)
                .case_insensitive(!query.case_sensitive)
                .build()
                .map(Self::Pattern)
                .map_err(|error| envelope::usage(format!("not a valid pattern: {error}")));
        }

        Ok(Self::Literal {
            needle: if query.case_sensitive {
                query.pattern.clone()
            } else {
                query.pattern.to_lowercase()
            },
            case_sensitive: query.case_sensitive,
        })
    }

    fn matches(&self, text: &str) -> bool {
        match self {
            Self::Literal {
                needle,
                case_sensitive,
            } => {
                if *case_sensitive {
                    text.contains(needle.as_str())
                } else {
                    text.to_lowercase().contains(needle.as_str())
                }
            }
            Self::Pattern(pattern) => pattern.is_match(text),
        }
    }
}

impl Analysis {
    pub fn search(&self, query: &SearchQuery, budget: &Budget) -> eyre::Result<SearchData> {
        let matcher = Matcher::new(query)?;
        let in_range = |mach_ns: u64| {
            query.from_mach_ns.is_none_or(|from| mach_ns >= from)
                && query.to_mach_ns.is_none_or(|to| mach_ns <= to)
        };
        let mut hits: Vec<Hit> = Vec::new();
        let mut searched = vec!["komorebi-log", "events", "recorder", "annotations"];

        for line in &self.log().lines {
            let Some(mach_ns) = line
                .wall_us
                .and_then(|wall_us| self.clock().wall_to_mach(wall_us))
            else {
                continue;
            };
            let spans: Vec<&str> = line.spans.iter().map(|span| span.name.as_str()).collect();
            let haystack = format!(
                "{} {}: {}",
                spans.join(":"),
                line.target.as_deref().unwrap_or(""),
                line.message
            );
            // the message alone, so `^processing` anchors mean what they say, or with its spans and module
            if in_range(mach_ns) && (matcher.matches(&line.message) || matcher.matches(&haystack)) {
                hits.push(Hit {
                    source: "komorebi-log".to_string(),
                    video_s: self.video_s(mach_ns),
                    mach_ns,
                    reference: format!("log:{}", line.number),
                    text: line.message.clone(),
                });
            }
        }

        for event in self.events() {
            let haystack = format!(
                "{} {}",
                event.info.summary(),
                event.info.argument.as_deref().unwrap_or("")
            );
            if in_range(event.recv_mach_ns) && matcher.matches(&haystack) {
                hits.push(Hit {
                    source: "events".to_string(),
                    video_s: self.video_s(event.recv_mach_ns),
                    mach_ns: event.recv_mach_ns,
                    reference: format!("event:{}", event.seq),
                    text: event.info.summary(),
                });
            }
        }

        for incident in self.incidents() {
            let mach_ns = self.manifest.mach_at_video_seconds(incident.video_s);
            let text = format!("{}: {}", incident.kind, incident.detail);
            if in_range(mach_ns) && matcher.matches(&text) {
                hits.push(Hit {
                    source: "recorder".to_string(),
                    video_s: incident.video_s,
                    mach_ns,
                    reference: format!("incident:{}", incident.number),
                    text,
                });
            }
        }

        for annotation in annotations::load(&self.session.dir)?.annotations {
            let text = format!(
                "{} {} {}",
                annotation.title,
                annotation.text,
                annotation.expected.clone().unwrap_or_default()
            );
            if in_range(annotation.start_mach_ns) && matcher.matches(&text) {
                hits.push(Hit {
                    source: "annotations".to_string(),
                    video_s: self.video_s(annotation.start_mach_ns),
                    mach_ns: annotation.start_mach_ns,
                    reference: annotation.id.clone(),
                    text: text.trim().to_string(),
                });
            }
        }

        if query.oslog {
            searched.push("oslog");
            let from_s = query
                .from_mach_ns
                .map_or(f64::MIN / 2.0, |ns| self.video_s(ns));
            let to_s = query
                .to_mach_ns
                .map_or(f64::MAX / 2.0, |ns| self.video_s(ns));
            // the timeline index reads each record's raw text; stop when the budget says to
            let records = timeline::query(
                &self.session,
                &self.manifest,
                from_s.max(-1e6),
                to_s.min(1e9),
                &Filter {
                    app: None,
                    sources: vec!["oslog".to_string()],
                },
            )?;
            for record in records {
                budget
                    .charge(record.text.len() as u64)
                    .map_err(|exceeded| {
                        envelope::over_budget(format!(
                            "the search {exceeded}; narrow it with --from/--to"
                        ))
                    })?;
                if matcher.matches(&record.text) {
                    hits.push(Hit {
                        source: record.entry.source.clone(),
                        video_s: record.entry.video_s,
                        mach_ns: record.entry.mach_ns,
                        reference: format!("oslog:{}:{}", record.entry.file, record.entry.line),
                        text: record.text,
                    });
                }
            }
        }

        hits.sort_by(|a, b| {
            a.mach_ns
                .cmp(&b.mach_ns)
                .then(a.reference.cmp(&b.reference))
        });
        let matching = hits.len().saturating_sub(query.skip);
        let page: Vec<Hit> = hits
            .into_iter()
            .skip(query.skip)
            .take(query.limit.unwrap_or(usize::MAX))
            .collect();

        Ok(SearchData {
            query: query.pattern.clone(),
            hits: page,
            matching,
            searched: searched.into_iter().map(str::to_string).collect(),
        })
    }
}
