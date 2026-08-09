//! Completion-report validation: prove a remediation agent's YAML/JSON report
//! is consistent with the recorded events before it is returned.
//!
//! Every claim in the report must trace to a recorded event (constitution:
//! evidence has authority). The validator reports ALL mismatches in one pass
//! so the agent can repair once, not iterate.

use crate::error::SnagError;
use crate::parser::{
    MAX_COMPLETION_COMMITS, MAX_COMPLETION_ITEMS, MAX_COMPLETION_RELATIONSHIPS,
    MAX_COMPLETION_TASKS, MAX_INTAKE_BYTES, read_bounded, validate_string, validate_yaml_nesting,
};
use crate::remediation::events::*;
use crate::remediation::reducer;
use crate::remediation::reducer::STATE_VERIFIED_FIXED;
use crate::store::Store;
use anyhow::Result;
use rusqlite::OptionalExtension;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct CompletionReport {
    pub snag_remediation: RemediationSection,
}

#[derive(Debug, Deserialize)]
pub struct RemediationSection {
    pub reviewed: Vec<ReviewedItem>,
}

#[derive(Debug, Deserialize)]
pub struct ReviewedItem {
    pub observation_id: String,
    pub disposition: String,
    #[serde(default)]
    pub relationships: Vec<ReportRelationship>,
    pub finding_id: Option<String>,
    #[serde(default)]
    pub task_ids: Vec<String>,
    #[serde(default)]
    pub commits: Vec<ReportCommit>,
    pub verification: Option<ReportVerification>,
    pub result: Option<String>,
    pub duplicate_of: Option<String>,
    pub rationale: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReportRelationship {
    pub relation: String,
    pub observation_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ReportCommit {
    pub repository_id: String,
    pub sha: String,
}

#[derive(Debug, Deserialize)]
pub struct ReportVerification {
    pub receipt: String,
    pub status: String,
}

/// A per-item validation failure, formatted for the agent.
#[derive(Debug)]
pub struct ReportFailure {
    pub observation_id: String,
    pub message: String,
}

fn load_report(report_path: &std::path::Path) -> Result<CompletionReport> {
    let file = std::fs::File::open(report_path)
        .map_err(|e| anyhow::anyhow!("cannot read report {}: {e}", report_path.display()))?;
    let content =
        read_bounded(file, MAX_INTAKE_BYTES, "completion report").map_err(anyhow::Error::from)?;
    validate_yaml_nesting(&content).map_err(anyhow::Error::from)?;
    let report: CompletionReport = serde_yaml::from_str(&content)
        .map_err(|e| anyhow::anyhow!("cannot parse report (YAML or JSON): {e}"))?;
    validate_report(&report).map_err(anyhow::Error::from)?;
    Ok(report)
}

fn validate_report(report: &CompletionReport) -> Result<(), SnagError> {
    let reviewed = &report.snag_remediation.reviewed;
    if reviewed.len() > MAX_COMPLETION_ITEMS {
        return Err(SnagError::Validation(format!(
            "completion report reviewed items exceed the {MAX_COMPLETION_ITEMS}-item limit"
        )));
    }
    for item in reviewed {
        validate_item(item)?;
    }
    Ok(())
}

fn validate_item(item: &ReviewedItem) -> Result<(), SnagError> {
    validate_item_strings(item)?;
    validate_relationships(item)?;
    validate_tasks(item)?;
    validate_commits(item)?;
    validate_verification(item)
}

fn validate_item_strings(item: &ReviewedItem) -> Result<(), SnagError> {
    for (name, value) in [
        ("observation_id", &item.observation_id),
        ("disposition", &item.disposition),
    ] {
        validate_string(name, value)?;
    }
    for (name, value) in [
        ("finding_id", item.finding_id.as_ref()),
        ("result", item.result.as_ref()),
        ("duplicate_of", item.duplicate_of.as_ref()),
        ("rationale", item.rationale.as_ref()),
    ] {
        if let Some(value) = value {
            validate_string(name, value)?;
        }
    }
    Ok(())
}

fn validate_relationships(item: &ReviewedItem) -> Result<(), SnagError> {
    if item.relationships.len() > MAX_COMPLETION_RELATIONSHIPS {
        return Err(SnagError::Validation(format!(
            "completion report relationships exceed the {MAX_COMPLETION_RELATIONSHIPS}-item limit"
        )));
    }
    for relationship in &item.relationships {
        validate_string("relationship", &relationship.relation)?;
        validate_string("relationship observation_id", &relationship.observation_id)?;
    }
    Ok(())
}

fn validate_tasks(item: &ReviewedItem) -> Result<(), SnagError> {
    if item.task_ids.len() > MAX_COMPLETION_TASKS {
        return Err(SnagError::Validation(format!(
            "completion report task_ids exceed the {MAX_COMPLETION_TASKS}-item limit"
        )));
    }
    for task in &item.task_ids {
        validate_string("task_id", task)?;
    }
    Ok(())
}

fn validate_commits(item: &ReviewedItem) -> Result<(), SnagError> {
    if item.commits.len() > MAX_COMPLETION_COMMITS {
        return Err(SnagError::Validation(format!(
            "completion report commits exceed the {MAX_COMPLETION_COMMITS}-item limit"
        )));
    }
    for commit in &item.commits {
        validate_string("commit repository_id", &commit.repository_id)?;
        validate_string("commit sha", &commit.sha)?;
    }
    Ok(())
}

fn validate_verification(item: &ReviewedItem) -> Result<(), SnagError> {
    if let Some(verification) = &item.verification {
        validate_string("verification receipt", &verification.receipt)?;
        validate_string("verification status", &verification.status)?;
    }
    Ok(())
}

/// Number of reviewed items in the report (for the success summary).
pub fn item_count(report_path: &std::path::Path) -> Result<usize> {
    Ok(load_report(report_path)?.snag_remediation.reviewed.len())
}

/// Load and validate a completion report. Returns the list of failures (empty
/// means the report is consistent with the recorded events).
pub fn verify_report(store: &Store, report_path: &std::path::Path) -> Result<Vec<ReportFailure>> {
    let report = load_report(report_path)?;
    let mut failures = Vec::new();
    for item in &report.snag_remediation.reviewed {
        check_item(store, item, &mut failures);
    }
    Ok(failures)
}

fn check_item(store: &Store, item: &ReviewedItem, failures: &mut Vec<ReportFailure>) {
    // The observation must exist before any recorded-state comparison.
    let exists: bool = store
        .conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM observations WHERE observation_id = ?1)",
            rusqlite::params![item.observation_id],
            |row| row.get(0),
        )
        .unwrap_or(false);
    if !exists {
        fail(
            failures,
            &item.observation_id,
            format!("observation {} does not exist", item.observation_id),
        );
        return;
    }

    let reduced = match reducer::reduce_observation(&store.conn, &item.observation_id) {
        Ok(r) => r,
        Err(e) => {
            fail(
                failures,
                &item.observation_id,
                format!("cannot reduce observation: {e}"),
            );
            return;
        }
    };

    check_disposition(store, item, &reduced, failures);
    check_duplicate(item, &reduced, failures);
    check_finding(item, &reduced, failures);
    check_tasks(store, item, failures);
    check_commits(store, item, failures);
    check_verification(store, item, failures);
    check_result(item, &reduced, failures);
    check_relationships(store, item, failures);
}

/// Append one failure to the report.
fn fail(failures: &mut Vec<ReportFailure>, observation_id: &str, message: String) {
    failures.push(ReportFailure {
        observation_id: observation_id.to_string(),
        message,
    });
}

/// The reported disposition must be in the vocabulary and match the recorded
/// current disposition.
fn check_disposition(
    _store: &Store,
    item: &ReviewedItem,
    reduced: &reducer::ReducedObservation,
    failures: &mut Vec<ReportFailure>,
) {
    let disposition = item.disposition.replace('-', "_");
    if !DISPOSITIONS.contains(&disposition.as_str()) {
        fail(
            failures,
            &item.observation_id,
            format!("unknown disposition '{}'", item.disposition),
        );
        return;
    }
    match reduced.disposition.as_deref() {
        Some(recorded) if recorded == disposition => {}
        Some(recorded) => fail(
            failures,
            &item.observation_id,
            format!("disposition mismatch: report says {disposition}, recorded {recorded}"),
        ),
        None => fail(
            failures,
            &item.observation_id,
            format!("disposition mismatch: report says {disposition}, recorded none"),
        ),
    }
}

/// duplicate_of must match the recorded disposition target.
fn check_duplicate(
    item: &ReviewedItem,
    reduced: &reducer::ReducedObservation,
    failures: &mut Vec<ReportFailure>,
) {
    let disposition = item.disposition.replace('-', "_");
    if let Some(dof) = &item.duplicate_of {
        if disposition != DISP_DUPLICATE {
            fail(
                failures,
                &item.observation_id,
                "duplicate_of reported on a non-duplicate disposition".to_string(),
            );
        } else if reduced.disposition_target.as_deref() != Some(dof) {
            fail(
                failures,
                &item.observation_id,
                format!(
                    "duplicate_of mismatch: report says {dof}, recorded {}",
                    reduced.disposition_target.as_deref().unwrap_or("none")
                ),
            );
        }
    } else if disposition == DISP_DUPLICATE {
        fail(
            failures,
            &item.observation_id,
            "duplicate disposition reported without duplicate_of".to_string(),
        );
    }
}

/// finding_id must match the recorded promotion.
fn check_finding(
    item: &ReviewedItem,
    reduced: &reducer::ReducedObservation,
    failures: &mut Vec<ReportFailure>,
) {
    if let Some(fid) = &item.finding_id
        && reduced.promoted_finding_id.as_deref() != Some(fid)
    {
        fail(
            failures,
            &item.observation_id,
            format!(
                "finding_id mismatch: report says {fid}, recorded {}",
                reduced.promoted_finding_id.as_deref().unwrap_or("none")
            ),
        );
    }
}

/// Every reported task id must be a recorded task link.
fn check_tasks(store: &Store, item: &ReviewedItem, failures: &mut Vec<ReportFailure>) {
    for tid in &item.task_ids {
        let linked: bool = store
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM remediation_links
                 WHERE observation_id = ?1 AND link_type = 'task' AND target_id = ?2)",
                rusqlite::params![item.observation_id, tid],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !linked {
            fail(
                failures,
                &item.observation_id,
                format!("task {tid} is not recorded as a task link"),
            );
        }
    }
}

/// Every reported commit must be a recorded commit link.
fn check_commits(store: &Store, item: &ReviewedItem, failures: &mut Vec<ReportFailure>) {
    for c in &item.commits {
        let linked: bool = store
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM remediation_links
                 WHERE observation_id = ?1 AND link_type = 'commit'
                   AND target_id = ?2 AND repository_id = ?3)",
                rusqlite::params![item.observation_id, c.sha, c.repository_id],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !linked {
            fail(
                failures,
                &item.observation_id,
                format!(
                    "commit {}@{} is not recorded as a commit link",
                    c.repository_id, c.sha
                ),
            );
        }
    }
}

/// The verification receipt must be recorded with the reported status.
fn check_verification(store: &Store, item: &ReviewedItem, failures: &mut Vec<ReportFailure>) {
    if let Some(v) = &item.verification {
        let recorded_status: Option<String> = store
            .conn
            .query_row(
                "SELECT status FROM remediation_links
                 WHERE observation_id = ?1 AND link_type = 'verification' AND target_id = ?2
                 ORDER BY source_record_sequence DESC LIMIT 1",
                rusqlite::params![item.observation_id, v.receipt],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten();
        match recorded_status {
            Some(s) if s == v.status => {}
            Some(s) => fail(
                failures,
                &item.observation_id,
                format!(
                    "verification status mismatch: report says {} for receipt {}, recorded {s}",
                    v.status, v.receipt
                ),
            ),
            None => fail(
                failures,
                &item.observation_id,
                format!("verification receipt {} is not recorded", v.receipt),
            ),
        }
    }
}

/// The reported result must match the derived state; verified_fixed requires
/// an accepted receipt.
fn check_result(
    item: &ReviewedItem,
    reduced: &reducer::ReducedObservation,
    failures: &mut Vec<ReportFailure>,
) {
    if let Some(result) = &item.result {
        let result = result.replace('-', "_");
        if result != reduced.state {
            fail(
                failures,
                &item.observation_id,
                format!(
                    "result mismatch: report says {result}, derived state is {}",
                    reduced.state
                ),
            );
        }
        if result == STATE_VERIFIED_FIXED
            && reduced.latest_verification_status.as_deref() != Some(VERIFY_ACCEPTED)
        {
            fail(
                failures,
                &item.observation_id,
                "verified_fixed reported without an accepted verification receipt".to_string(),
            );
        }
    }
}

/// Every reported relationship must be a live recorded assertion (either
/// canonical direction for symmetric relations).
fn check_relationships(store: &Store, item: &ReviewedItem, failures: &mut Vec<ReportFailure>) {
    for rel in &item.relationships {
        let relation = rel.relation.replace('-', "_");
        if !RELATIONSHIPS.contains(&relation.as_str()) {
            fail(
                failures,
                &item.observation_id,
                format!("unknown relation '{}'", rel.relation),
            );
            continue;
        }
        let (left, right) = if SYMMETRIC_RELATIONSHIPS.contains(&relation.as_str())
            && item.observation_id > rel.observation_id
        {
            (rel.observation_id.clone(), item.observation_id.clone())
        } else {
            (item.observation_id.clone(), rel.observation_id.clone())
        };
        let live: bool = store
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM observation_relationships
                 WHERE left_observation_id = ?1 AND right_observation_id = ?2
                   AND relation = ?3 AND retracted_by_record_sequence IS NULL)",
                rusqlite::params![left, right, relation],
                |row| row.get(0),
            )
            .unwrap_or(false);
        if !live {
            fail(
                failures,
                &item.observation_id,
                format!(
                    "relationship {relation} with {} is not recorded",
                    rel.observation_id
                ),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> ReviewedItem {
        ReviewedItem {
            observation_id: "obs".to_string(),
            disposition: "fixed".to_string(),
            relationships: Vec::new(),
            finding_id: None,
            task_ids: Vec::new(),
            commits: Vec::new(),
            verification: None,
            result: None,
            duplicate_of: None,
            rationale: None,
        }
    }

    #[test]
    fn completion_report_rejects_item_limit_plus_one() {
        let report = CompletionReport {
            snag_remediation: RemediationSection {
                reviewed: std::iter::repeat_with(item)
                    .take(MAX_COMPLETION_ITEMS + 1)
                    .collect(),
            },
        };
        assert!(matches!(
            validate_report(&report),
            Err(SnagError::Validation(_))
        ));
    }

    #[test]
    fn completion_report_accepts_item_boundary() {
        let report = CompletionReport {
            snag_remediation: RemediationSection {
                reviewed: std::iter::repeat_with(item)
                    .take(MAX_COMPLETION_ITEMS)
                    .collect(),
            },
        };
        assert!(validate_report(&report).is_ok());
    }

    #[test]
    fn completion_report_rejects_oversized_string() {
        let mut oversized = item();
        oversized.rationale = Some("x".repeat(crate::parser::MAX_STRING_BYTES + 1));
        let report = CompletionReport {
            snag_remediation: RemediationSection {
                reviewed: vec![oversized],
            },
        };
        assert!(matches!(
            validate_report(&report),
            Err(SnagError::Validation(_))
        ));
    }
}
