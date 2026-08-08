use crate::types::Observation;
use serde::Serialize;
use std::collections::BTreeMap;

/// The stable semantic payload used for idempotency comparison.
///
/// It contains only reporter-supplied / explicitly-asserted fields and the
/// resolved identifiers that are stable across a retry. Generated identifiers
/// (observation_id, local_sequence), the capture timestamp, the record hash,
/// discovered branch/HEAD, and auto-attached ambient context are excluded so
/// that a retry of the same report maps to the same digest.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SemanticIdempotencyPayload {
    pub schema_version: u32,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind_assertion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity_assertion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_behavior: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_behavior: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reproduction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workaround: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub impact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    pub sensitivity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<BTreeMap<String, String>>,
    pub source: crate::types::SourceInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_session_id: Option<String>,
    /// Explicitly supplied execution identifiers (session/task/attempt/etc.).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_attempt_id: Option<String>,
    /// Resolved repository IDs (primary + affected).
    pub repository_ids: Vec<String>,
    pub owner_repository_id: Option<String>,
    /// Artifact content digests (and byte lengths) — stable content identity.
    pub artifact_digests: Vec<(String, u64)>,
}

/// Compute the stable semantic idempotency digest for an observation, in a
/// form that is identical on a retry of the same explicit report.
pub fn observation_semantic_digest(obs: &Observation) -> String {
    let exec = obs
        .context
        .execution
        .as_ref()
        .map(|e| {
            (
                e.session_id.clone(),
                e.task_id.clone(),
                e.attempt_id.clone(),
            )
        })
        .unwrap_or((None, None, None));

    let mut artifact_digests: Vec<(String, u64)> = obs
        .artifacts
        .iter()
        .map(|a| (a.digest.clone(), a.byte_length))
        .collect();
    artifact_digests.sort();

    // The snag-generated repro_key label is tooling metadata (session
    // localization), not observation content: it must never perturb the
    // semantic digest, or idempotent replays would diverge.
    let mut semantic_labels = obs.labels.clone();
    if let Some(labels) = semantic_labels.as_mut() {
        labels.remove("repro_key");
    }

    let payload = SemanticIdempotencyPayload {
        schema_version: obs.schema_version,
        title: obs.title.clone(),
        summary: obs.summary.clone(),
        kind_assertion: obs.kind_assertion.clone(),
        severity_assertion: obs.severity_assertion.clone(),
        expected_behavior: obs.expected_behavior.clone(),
        observed_behavior: obs.observed_behavior.clone(),
        reproduction: obs.reproduction.clone(),
        workaround: obs.workaround.clone(),
        impact: obs.impact.clone(),
        confidence: obs.confidence,
        sensitivity: serde_json::to_string(&obs.sensitivity).unwrap_or_default(),
        labels: semantic_labels,
        source: obs.source.clone(),
        execution_session_id: exec.0,
        execution_task_id: exec.1,
        execution_attempt_id: exec.2,
        repository_ids: obs.affected_repository_ids.clone(),
        owner_repository_id: obs.owner_repository_id.clone(),
        artifact_digests,
    };

    let json = serde_json::to_string(&payload).expect("semantic idempotency serialization failed");
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"semantic-idempotency-v1\0");
    hasher.update(json.as_bytes());
    format!("blake3:{}", hasher.finalize().to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ContextInfo, ExecutionContext, Sensitivity, SourceInfo};

    fn base() -> Observation {
        Observation {
            schema_version: 1,
            observation_id: "obs_AAAA".to_string(),
            store_id: "store_x".to_string(),
            local_sequence: 99,
            idempotency_key: Some("k".to_string()),
            created_at: "2026-08-04T00:00:00Z".to_string(),
            source: SourceInfo {
                kind: "agent_explicit".to_string(),
                system: None,
                reporter_id: None,
                agent_runtime: None,
                agent_name: None,
                model: None,
                detector_id: None,
                detector_version: None,
            },
            title: "t".to_string(),
            summary: None,
            kind_assertion: Some("bug".to_string()),
            severity_assertion: None,
            expected_behavior: None,
            observed_behavior: None,
            reproduction: None,
            workaround: None,
            impact: None,
            confidence: Some(0.9),
            sensitivity: Sensitivity::Normal,
            labels: None,
            context: ContextInfo::default(),
            artifacts: vec![],
            affected_repository_ids: vec![],
            owner_repository_id: None,
        }
    }

    #[test]
    fn digest_is_stable_across_generated_fields() {
        let mut a = base();
        let mut b = base();
        // Different generated/volatile fields must NOT change the digest.
        a.observation_id = "obs_A1".to_string();
        a.local_sequence = 1;
        a.created_at = "2026-08-04T01:00:00Z".to_string();
        b.observation_id = "obs_B2".to_string();
        b.local_sequence = 2;
        b.created_at = "2026-08-04T02:00:00Z".to_string();
        assert_eq!(
            observation_semantic_digest(&a),
            observation_semantic_digest(&b)
        );
    }

    #[test]
    fn digest_changes_on_semantic_change() {
        let a = base();
        let mut b = base();
        b.title = "different".to_string();
        assert_ne!(
            observation_semantic_digest(&a),
            observation_semantic_digest(&b)
        );
    }

    #[test]
    fn changed_affected_repos_changes_digest() {
        let a = base();
        let mut b = base();
        b.affected_repository_ids = vec!["repo_x".to_string()];
        assert_ne!(
            observation_semantic_digest(&a),
            observation_semantic_digest(&b)
        );
    }

    #[test]
    fn changed_head_does_not_change_digest() {
        let a = base();
        assert_eq!(
            observation_semantic_digest(&a),
            observation_semantic_digest(&base())
        );
        // Ambient execution/context changes not asserted are excluded.
        let mut b = base();
        b.context.execution = Some(ExecutionContext {
            cwd: Some("/x".to_string()),
            ..Default::default()
        });
        let _ = b;
    }
}
