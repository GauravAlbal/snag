use crate::error::SnagError;
use crate::types::{ContextInfo, Sensitivity, SourceInfo};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read;

/// Intake budgets shared by report, context-file, and remediation readers.
/// Inputs are bounded before deserialization; strings and collections are
/// validated again after deserialization so attacker-controlled fields cannot
/// become oversized persisted values.
/// Total payloads are limited to 1 MiB and individual persisted strings to
/// 64 KiB. Labels, repositories, and artifacts are capped at 64 items;
/// completion reports allow 256 reviewed items and 64 entries per nested list.
/// Arbitrary context JSON is capped at 16 nesting levels and 256 entries.
pub const MAX_INTAKE_BYTES: usize = 1024 * 1024;
pub const MAX_STRING_BYTES: usize = 64 * 1024;
pub const MAX_LABELS: usize = 64;
pub const MAX_REPOSITORIES: usize = 64;
pub const MAX_ARTIFACTS: usize = 64;
pub const MAX_CONTEXT_NESTING_DEPTH: usize = 16;
pub const MAX_CONTEXT_EXTRA_ENTRIES: usize = 256;
pub const MAX_COMPLETION_ITEMS: usize = 256;
pub const MAX_COMPLETION_RELATIONSHIPS: usize = 64;
pub const MAX_COMPLETION_TASKS: usize = 64;
pub const MAX_COMPLETION_COMMITS: usize = 64;

/// Read at most `limit + 1` bytes, returning a typed validation error when the
/// input exceeds the documented budget. This must be used before serde parses
/// attacker- or agent-controlled files and streams.
pub fn read_bounded<R: Read>(reader: R, limit: usize, label: &str) -> Result<String, SnagError> {
    let mut bytes = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| SnagError::Validation(format!("could not read {label}: {e}")))?;
    if bytes.len() > limit {
        return Err(SnagError::Validation(format!(
            "{label} exceeds the {}-byte limit",
            limit
        )));
    }
    String::from_utf8(bytes)
        .map_err(|_| SnagError::Validation(format!("{label} must be valid UTF-8")))
}

/// Reject deeply nested JSON before serde recursively constructs values. This
/// scan is intentionally lexical, so braces in strings do not affect depth.
pub fn validate_json_nesting(input: &str) -> Result<(), SnagError> {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for byte in input.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            continue;
        }
        match byte {
            b'"' => quoted = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_CONTEXT_NESTING_DEPTH {
                    return Err(SnagError::Validation(format!(
                        "JSON nesting exceeds the {}-level limit",
                        MAX_CONTEXT_NESTING_DEPTH
                    )));
                }
            }
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// Reject deeply indented YAML and deeply nested flow collections before
/// serde_yaml recursively constructs completion-report values.
pub fn validate_yaml_nesting(input: &str) -> Result<(), SnagError> {
    let mut flow_depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for line in input.lines() {
        let indentation = line
            .bytes()
            .take_while(|byte| matches!(byte, b' ' | b'\t'))
            .count();
        if indentation / 2 + 1 > MAX_CONTEXT_NESTING_DEPTH {
            return Err(SnagError::Validation(format!(
                "YAML nesting exceeds the {}-level limit",
                MAX_CONTEXT_NESTING_DEPTH
            )));
        }
        for byte in line.bytes().skip(indentation) {
            if quoted {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    quoted = false;
                }
                continue;
            }
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    flow_depth += 1;
                    if flow_depth > MAX_CONTEXT_NESTING_DEPTH {
                        return Err(SnagError::Validation(format!(
                            "YAML nesting exceeds the {}-level limit",
                            MAX_CONTEXT_NESTING_DEPTH
                        )));
                    }
                }
                b'}' | b']' => flow_depth = flow_depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    Ok(())
}

pub fn validate_string(name: &str, value: &str) -> Result<(), SnagError> {
    if value.len() > MAX_STRING_BYTES {
        return Err(SnagError::Validation(format!(
            "{name} exceeds the {}-byte string limit",
            MAX_STRING_BYTES
        )));
    }
    Ok(())
}

fn validate_optional_string(name: &str, value: Option<&String>) -> Result<(), SnagError> {
    if let Some(value) = value {
        validate_string(name, value)?;
    }
    Ok(())
}

fn validate_context_value(
    value: &serde_json::Value,
    depth: usize,
    entries: &mut usize,
) -> Result<(), SnagError> {
    if depth > MAX_CONTEXT_NESTING_DEPTH {
        return Err(SnagError::Validation(format!(
            "context nesting exceeds the {}-level limit",
            MAX_CONTEXT_NESTING_DEPTH
        )));
    }
    match value {
        serde_json::Value::String(value) => validate_string("context string", value)?,
        serde_json::Value::Array(values) => {
            *entries = entries.saturating_add(values.len());
            for value in values {
                validate_context_value(value, depth + 1, entries)?;
            }
        }
        serde_json::Value::Object(values) => {
            *entries = entries.saturating_add(values.len());
            for (key, value) in values {
                validate_string("context key", key)?;
                validate_context_value(value, depth + 1, entries)?;
            }
        }
        _ => {}
    }
    if *entries > MAX_CONTEXT_EXTRA_ENTRIES {
        return Err(SnagError::Validation(format!(
            "context extra entries exceed the {}-entry limit",
            MAX_CONTEXT_EXTRA_ENTRIES
        )));
    }
    Ok(())
}

pub fn validate_source(source: &SourceInfo) -> Result<(), SnagError> {
    validate_string("source.kind", &source.kind)?;
    validate_optional_string("source.system", source.system.as_ref())?;
    validate_optional_string("source.reporter_id", source.reporter_id.as_ref())?;
    validate_optional_string("source.agent_runtime", source.agent_runtime.as_ref())?;
    validate_optional_string("source.agent_name", source.agent_name.as_ref())?;
    validate_optional_string("source.model", source.model.as_ref())?;
    validate_optional_string("source.detector_id", source.detector_id.as_ref())?;
    validate_optional_string("source.detector_version", source.detector_version.as_ref())
}

pub fn validate_context_parts(
    repository: Option<&crate::types::RepositoryContext>,
    execution: Option<&crate::types::ExecutionContext>,
    extra: Option<&serde_json::Value>,
) -> Result<(), SnagError> {
    if let Some(repository) = repository {
        for (name, value) in [
            (
                "context.repository.repository_id",
                repository.repository_id.as_ref(),
            ),
            (
                "context.repository.checkout_id",
                repository.checkout_id.as_ref(),
            ),
            (
                "context.repository.worktree_id",
                repository.worktree_id.as_ref(),
            ),
            (
                "context.repository.repository_root",
                repository.repository_root.as_ref(),
            ),
            (
                "context.repository.git_common_dir",
                repository.git_common_dir.as_ref(),
            ),
            ("context.repository.git_head", repository.git_head.as_ref()),
            (
                "context.repository.git_branch",
                repository.git_branch.as_ref(),
            ),
            (
                "context.repository.relative_cwd",
                repository.relative_cwd.as_ref(),
            ),
        ] {
            validate_optional_string(name, value)?;
        }
        if repository.git_remote_aliases.len() > MAX_REPOSITORIES {
            return Err(SnagError::Validation(format!(
                "context repository aliases exceed the {MAX_REPOSITORIES}-item limit"
            )));
        }
        for value in &repository.git_remote_aliases {
            validate_string("context.repository.git_remote_alias", value)?;
        }
    }
    if let Some(execution) = execution {
        for (name, value) in [
            ("context.execution.cwd", execution.cwd.as_ref()),
            (
                "context.execution.workspace_id",
                execution.workspace_id.as_ref(),
            ),
            (
                "context.execution.program_id",
                execution.program_id.as_ref(),
            ),
            (
                "context.execution.session_id",
                execution.session_id.as_ref(),
            ),
            ("context.execution.task_id", execution.task_id.as_ref()),
            (
                "context.execution.attempt_id",
                execution.attempt_id.as_ref(),
            ),
            ("context.execution.tool_name", execution.tool_name.as_ref()),
            (
                "context.execution.tool_invocation_id",
                execution.tool_invocation_id.as_ref(),
            ),
            (
                "context.execution.command_shape",
                execution.command_shape.as_ref(),
            ),
        ] {
            validate_optional_string(name, value)?;
        }
    }
    if let Some(extra) = extra {
        let mut entries = 0;
        validate_context_value(extra, 1, &mut entries)?;
    }
    Ok(())
}

fn validate_context(context: &ContextInfo) -> Result<(), SnagError> {
    validate_context_parts(
        context.repository.as_ref(),
        context.execution.as_ref(),
        context.extra.as_ref(),
    )
}

/// Validate all fields that can be supplied by JSON observation intake.
pub fn validate_json_input(input: &JsonInput) -> Result<(), SnagError> {
    for (name, value) in [
        ("title", input.title.as_ref()),
        ("summary", input.summary.as_ref()),
        ("kind_assertion", input.kind_assertion.as_ref()),
        ("severity_assertion", input.severity_assertion.as_ref()),
        ("expected_behavior", input.expected_behavior.as_ref()),
        ("observed_behavior", input.observed_behavior.as_ref()),
        ("reproduction", input.reproduction.as_ref()),
        ("workaround", input.workaround.as_ref()),
        ("impact", input.impact.as_ref()),
        ("sensitivity", input.sensitivity.as_ref()),
        ("idempotency_key", input.idempotency_key.as_ref()),
        ("owner", input.owner.as_ref()),
    ] {
        validate_optional_string(name, value)?;
    }
    if let Some(labels) = &input.labels {
        validate_labels(labels)?;
    }
    if let Some(source) = &input.source {
        validate_source(source)?;
    }
    if let Some(context) = &input.context {
        validate_context(context)?;
    }
    if let Some(values) = &input.artifacts {
        if values.len() > MAX_ARTIFACTS {
            return Err(SnagError::Validation(format!(
                "artifacts exceed the {MAX_ARTIFACTS}-item limit"
            )));
        }
        for value in values {
            validate_string("artifact path", value)?;
        }
    }
    if let Some(values) = &input.affected_repositories {
        if values.len() > MAX_REPOSITORIES {
            return Err(SnagError::Validation(format!(
                "affected repositories exceed the {MAX_REPOSITORIES}-item limit"
            )));
        }
        for value in values {
            validate_string("affected repository", value)?;
        }
    }
    Ok(())
}

fn validate_labels(labels: &BTreeMap<String, String>) -> Result<(), SnagError> {
    if labels.len() > MAX_LABELS {
        return Err(SnagError::Validation(format!(
            "labels exceed the {MAX_LABELS}-item limit"
        )));
    }
    for (key, value) in labels {
        validate_string("label key", key)?;
        validate_string("label value", value)?;
    }
    Ok(())
}

/// Validate fields extracted from prose intake.
pub fn validate_prose(input: &ProseInput) -> Result<(), SnagError> {
    for (name, value) in [
        ("title", Some(&input.title)),
        ("summary", input.summary.as_ref()),
        ("expected_behavior", input.expected.as_ref()),
        ("observed_behavior", input.observed.as_ref()),
        ("reproduction", input.repro.as_ref()),
        ("workaround", input.workaround.as_ref()),
        ("impact", input.impact.as_ref()),
        ("owner", input.owner.as_ref()),
        ("unowned", input.unowned.as_ref()),
    ] {
        validate_optional_string(name, value)?;
    }
    Ok(())
}

/// Complete JSON observation input (schema versions 1 and 2).
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
    /// Fix-owner repository declaration (schema version 2).
    pub owner: Option<String>,
    /// Explicitly declare that no fix-owner repository is known (schema version 2).
    pub unowned: Option<bool>,
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
    pub owner: Option<String>,
    pub unowned: Option<String>,
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
            "Owner:" => current_section = "Owner",
            "Unowned:" => current_section = "Unowned",
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
    input.owner = join_section("Owner");
    input.unowned = join_section("Unowned");

    input
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn bounded_reader_accepts_limit_and_rejects_limit_plus_one() {
        let exact = vec![b'x'; MAX_INTAKE_BYTES];
        assert_eq!(
            read_bounded(Cursor::new(exact), MAX_INTAKE_BYTES, "test")
                .unwrap()
                .len(),
            MAX_INTAKE_BYTES
        );
        let over = vec![b'x'; MAX_INTAKE_BYTES + 1];
        assert!(matches!(
            read_bounded(Cursor::new(over), MAX_INTAKE_BYTES, "test"),
            Err(SnagError::Validation(_))
        ));
    }
    #[test]
    fn json_budgets_reject_oversized_strings_and_collection_counts() {
        let oversized = JsonInput {
            title: Some("x".repeat(MAX_STRING_BYTES + 1)),
            ..Default::default()
        };
        assert!(matches!(
            validate_json_input(&oversized),
            Err(SnagError::Validation(_))
        ));
        let labels = (0..=MAX_LABELS)
            .map(|i| (format!("k{i}"), "v".to_string()))
            .collect();
        let too_many_labels = JsonInput {
            labels: Some(labels),
            ..Default::default()
        };
        assert!(matches!(
            validate_json_input(&too_many_labels),
            Err(SnagError::Validation(_))
        ));
        let too_many_repositories = JsonInput {
            affected_repositories: Some((0..=MAX_REPOSITORIES).map(|i| format!("r{i}")).collect()),
            ..Default::default()
        };
        assert!(matches!(
            validate_json_input(&too_many_repositories),
            Err(SnagError::Validation(_))
        ));
        let too_many_artifacts = JsonInput {
            artifacts: Some((0..=MAX_ARTIFACTS).map(|i| format!("a{i}")).collect()),
            ..Default::default()
        };
        assert!(matches!(
            validate_json_input(&too_many_artifacts),
            Err(SnagError::Validation(_))
        ));
    }

    #[test]
    fn json_budgets_accept_documented_boundaries() {
        let at_limit = JsonInput {
            title: Some("x".repeat(MAX_STRING_BYTES)),
            labels: Some(
                (0..MAX_LABELS)
                    .map(|i| (format!("k{i}"), "v".to_string()))
                    .collect(),
            ),
            artifacts: Some((0..MAX_ARTIFACTS).map(|i| format!("a{i}")).collect()),
            affected_repositories: Some((0..MAX_REPOSITORIES).map(|i| format!("r{i}")).collect()),
            ..Default::default()
        };
        assert!(validate_json_input(&at_limit).is_ok());
    }

    #[test]
    fn prose_budget_rejects_oversized_section() {
        let input = ProseInput {
            title: "title".to_string(),
            observed: Some("x".repeat(MAX_STRING_BYTES + 1)),
            ..Default::default()
        };
        assert!(matches!(
            validate_prose(&input),
            Err(SnagError::Validation(_))
        ));
    }

    #[test]
    fn json_nesting_rejects_limit_plus_one() {
        let nested = format!(
            "{}1{}",
            "[".repeat(MAX_CONTEXT_NESTING_DEPTH + 1),
            "]".repeat(MAX_CONTEXT_NESTING_DEPTH + 1)
        );
        assert!(matches!(
            validate_json_nesting(&nested),
            Err(SnagError::Validation(_))
        ));
    }

    #[test]
    fn yaml_nesting_rejects_limit_plus_one() {
        let nested = format!("{}key: value", " ".repeat(MAX_CONTEXT_NESTING_DEPTH * 2));
        assert!(matches!(
            validate_yaml_nesting(&nested),
            Err(SnagError::Validation(_))
        ));
    }
}
