use crate::types::{ContextInfo, Sensitivity, SourceInfo};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Complete JSON observation input (schema_version 1).
///
/// Unknown fields are ignored by serde by default; `deny_unknown_fields` is
/// intentionally NOT used so forward/extra fields follow a documented
/// compatibility rule (ignore unknown keys) rather than hard-failing on older
/// writers. All supported fields are wired into persistence by report.rs.
#[derive(Debug, Default, Deserialize)]
pub struct JsonInput {
    pub schema_version: Option<u32>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub kind_assertion: Option<String>,
    pub severity_assertion: Option<String>,
    pub expected_behavior: Option<String>,
    pub observed_behavior: Option<String>,
    pub reproduction: Option<String>,
    pub workaround: Option<String>,
    pub impact: Option<String>,
    /// Numeric confidence (0..=1).
    pub confidence: Option<f64>,
    pub sensitivity: Option<String>,
    pub labels: Option<BTreeMap<String, String>>,
    pub source: Option<SourceInfo>,
    pub context: Option<ContextInfo>,
    pub idempotency_key: Option<String>,
    /// Local file paths to ingest as artifacts.
    pub artifacts: Option<Vec<String>>,
    pub affected_repositories: Option<Vec<String>>,
}

/// Parse a `labels` value that may be either an object (`{"k":"v"}`) or an
/// array of strings (`["a","b"]`), normalizing to a key->value map.
#[allow(dead_code)]
pub fn labels_from_json(v: &serde_json::Value) -> Option<BTreeMap<String, String>> {
    match v {
        serde_json::Value::Object(map) => Some(
            map.iter()
                .map(|(k, val)| {
                    let s = match val {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    (k.clone(), s)
                })
                .collect(),
        ),
        serde_json::Value::Array(arr) => Some(
            arr.iter()
                .enumerate()
                .map(|(i, val)| (i.to_string(), val.as_str().unwrap_or("").to_string()))
                .collect(),
        ),
        _ => None,
    }
}

pub fn sensitivity_from_str(s: Option<&str>) -> Sensitivity {
    match s {
        Some("restricted") => Sensitivity::Restricted,
        Some("sensitive") => Sensitivity::Sensitive,
        _ => Sensitivity::Normal,
    }
}

// ---------------------------------------------------------------------------
// Asserted kind/severity vocabulary (v0).
// ---------------------------------------------------------------------------
// Report intake rejects values outside these sets so the corpus never drifts
// into ad-hoc kinds the queue ranking cannot rank. The list/review FILTERS
// stay permissive on purpose: legacy rows may legitimately carry
// pre-vocabulary values, and the filters are the only way to query them.

pub const KIND_BUG: &str = "bug";
pub const KIND_TOOLING: &str = "tooling";
pub const KIND_PAPERCUT: &str = "papercut";
pub const KIND_FRICTION: &str = "friction";
pub const KIND_USABILITY: &str = "usability";
pub const KIND_PROBE: &str = "probe";
pub const KIND_FEATURE: &str = "feature";

pub const KINDS: &[&str] = &[
    KIND_BUG,
    KIND_TOOLING,
    KIND_PAPERCUT,
    KIND_FRICTION,
    KIND_USABILITY,
    KIND_PROBE,
    KIND_FEATURE,
];

pub const SEV_BLOCKER: &str = "blocker";
pub const SEV_MAJOR: &str = "major";
pub const SEV_MEDIUM: &str = "medium";
pub const SEV_MINOR: &str = "minor";
pub const SEV_LOW: &str = "low";

pub const SEVERITIES: &[&str] = &[SEV_BLOCKER, SEV_MAJOR, SEV_MEDIUM, SEV_MINOR, SEV_LOW];

#[derive(Debug, Default)]
pub struct ProseInput {
    pub title: String,
    pub summary: Option<String>,
    pub expected: Option<String>,
    pub observed: Option<String>,
    pub repro: Option<String>,
    pub workaround: Option<String>,
    pub impact: Option<String>,
}

pub fn parse_prose(text: &str) -> ProseInput {
    let mut input = ProseInput::default();

    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return input;
    }

    let mut current_section = "Summary";
    let mut sections: std::collections::HashMap<&str, Vec<&str>> = std::collections::HashMap::new();

    let mut first_line_found = false;

    for line in lines {
        let trimmed = line.trim();
        if !first_line_found {
            if trimmed.is_empty() {
                continue;
            }
            input.title = trimmed.to_string();
            first_line_found = true;
            continue;
        }

        match trimmed {
            "Expected:" => current_section = "Expected",
            "Observed:" => current_section = "Observed",
            "Reproduction:" => current_section = "Reproduction",
            "Workaround:" => current_section = "Workaround",
            "Impact:" => current_section = "Impact",
            _ => {
                sections.entry(current_section).or_default().push(line);
            }
        }
    }

    let join_section = |name: &str| -> Option<String> {
        sections
            .get(name)
            .map(|v| v.join("\n").trim().to_string())
            .filter(|s| !s.is_empty())
    };

    input.summary = join_section("Summary");
    input.expected = join_section("Expected");
    input.observed = join_section("Observed");
    input.repro = join_section("Reproduction");
    input.workaround = join_section("Workaround");
    input.impact = join_section("Impact");

    input
}
