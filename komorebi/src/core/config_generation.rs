use clap::ValueEnum;
use serde::Deserialize;
use serde::Serialize;
use strum::Display;
use strum::EnumString;

use super::ApplicationIdentifier;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, Display, EnumString, ValueEnum)]
#[strum(serialize_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum ApplicationOptions {
    ObjectNameChange,
    Layered,
    TrayAndMultiWindow,
    Force,
    BorderOverflow,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MatchingRule {
    Simple(IdWithIdentifier),
    Composite(Vec<IdWithIdentifier>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceMatchingRule {
    pub monitor_index: usize,
    pub workspace_index: usize,
    pub matching_rule: MatchingRule,
    pub initial_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IdWithIdentifier {
    pub kind: ApplicationIdentifier,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matching_strategy: Option<MatchingStrategy>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Display)]
pub enum MatchingStrategy {
    Legacy,
    Equals,
    StartsWith,
    EndsWith,
    Contains,
    Regex,
    DoesNotEndWith,
    DoesNotStartWith,
    DoesNotEqual,
    DoesNotContain,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdWithIdentifierAndComment {
    pub kind: ApplicationIdentifier,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matching_strategy: Option<MatchingStrategy>,
}

impl From<IdWithIdentifierAndComment> for IdWithIdentifier {
    fn from(value: IdWithIdentifierAndComment) -> Self {
        Self {
            kind: value.kind,
            id: value.id.clone(),
            matching_strategy: value.matching_strategy,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplicationConfiguration {
    pub name: String,
    pub identifier: IdWithIdentifier,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<ApplicationOptions>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(alias = "float_identifiers")]
    pub ignore_identifiers: Option<Vec<MatchingRule>>,
}

impl ApplicationConfiguration {
    pub fn populate_default_matching_strategies(&mut self) {
        if self.identifier.matching_strategy.is_none() {
            match self.identifier.kind {
                ApplicationIdentifier::Exe | ApplicationIdentifier::Path => {
                    self.identifier.matching_strategy = Option::from(MatchingStrategy::Equals);
                }
                ApplicationIdentifier::Class | ApplicationIdentifier::Title => {}
            }
        }
    }
}
