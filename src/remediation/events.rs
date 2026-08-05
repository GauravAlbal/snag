//! Remediation event types.
//!
//! Every remediation mutation is an append-only record in the global record
//! stream (see `crate::record::RecordPayload`). Each event binds the
//! observation, reviewer identity, session identity, event-specific fields,
//! `created_at`, and — when the invoking command supplied one — an idempotency
//! key. The canonical record kernel hashes the full envelope, so remediation
//! events are covered by the same tamper-evidence as observations.

use crate::record::{CanonicalRecordV1, RecordPayload};
use crate::types::generate_id;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Record type names (must match the `record_type` column values).
// ---------------------------------------------------------------------------

pub const RECORD_CLAIMED: &str = "observation_claimed";
pub const RECORD_CLAIM_HEARTBEAT: &str = "observation_claim_heartbeat";
pub const RECORD_CLAIM_RELEASED: &str = "observation_claim_released";
pub const RECORD_CLAIM_EXPIRED: &str = "observation_claim_expired";
pub const RECORD_REVIEWED: &str = "observation_reviewed";
pub const RECORD_DISPOSITION_SET: &str = "observation_disposition_set";
pub const RECORD_REOPENED: &str = "observation_reopened";
pub const RECORD_RELATIONSHIP_ADDED: &str = "observation_relationship_added";
pub const RECORD_RELATIONSHIP_RETRACTED: &str = "observation_relationship_retracted";
pub const RECORD_PROMOTED: &str = "observation_promoted";
pub const RECORD_TASK_ATTACHED: &str = "remediation_task_attached";
pub const RECORD_FIX_ATTACHED: &str = "remediation_fix_attached";
pub const RECORD_VERIFICATION_ATTACHED: &str = "remediation_verification_attached";
pub const RECORD_MARKED_HANDLED: &str = "remediation_marked_handled";
pub const RECORD_REMEDIATION_REOPENED: &str = "remediation_reopened";

/// Every remediation record type. The reducer, rebuild, export reader-version
/// bump, and verify all key off membership here.
pub const REMEDIATION_RECORD_TYPES: &[&str] = &[
    RECORD_CLAIMED,
    RECORD_CLAIM_HEARTBEAT,
    RECORD_CLAIM_RELEASED,
    RECORD_CLAIM_EXPIRED,
    RECORD_REVIEWED,
    RECORD_DISPOSITION_SET,
    RECORD_REOPENED,
    RECORD_RELATIONSHIP_ADDED,
    RECORD_RELATIONSHIP_RETRACTED,
    RECORD_PROMOTED,
    RECORD_TASK_ATTACHED,
    RECORD_FIX_ATTACHED,
    RECORD_VERIFICATION_ATTACHED,
    RECORD_MARKED_HANDLED,
    RECORD_REMEDIATION_REOPENED,
];

// ---------------------------------------------------------------------------
// Dispositions (v0 vocabulary).
// ---------------------------------------------------------------------------

pub const DISP_CONFIRMED: &str = "confirmed";
pub const DISP_DUPLICATE: &str = "duplicate";
pub const DISP_EXPECTED_BEHAVIOR: &str = "expected_behavior";
pub const DISP_ENVIRONMENTAL: &str = "environmental";
pub const DISP_INSUFFICIENT_EVIDENCE: &str = "insufficient_evidence";
pub const DISP_DEFERRED: &str = "deferred";
pub const DISP_SUPERSEDED: &str = "superseded";

pub const DISPOSITIONS: &[&str] = &[
    DISP_CONFIRMED,
    DISP_DUPLICATE,
    DISP_EXPECTED_BEHAVIOR,
    DISP_ENVIRONMENTAL,
    DISP_INSUFFICIENT_EVIDENCE,
    DISP_DEFERRED,
    DISP_SUPERSEDED,
];

/// Dispositions that require a target observation.
pub const DISPOSITIONS_WITH_TARGET: &[&str] = &[DISP_DUPLICATE, DISP_SUPERSEDED];

/// Dispositions that are terminal negatives: the observation needs no
/// remediation work and is considered handled once the disposition applies.
pub const TERMINAL_NEGATIVE_DISPOSITIONS: &[&str] = &[
    DISP_DUPLICATE,
    DISP_EXPECTED_BEHAVIOR,
    DISP_ENVIRONMENTAL,
    DISP_INSUFFICIENT_EVIDENCE,
    DISP_SUPERSEDED,
];

// ---------------------------------------------------------------------------
// Relationships (v0 vocabulary).
// ---------------------------------------------------------------------------

pub const REL_SAME_FINDING: &str = "same_finding";
pub const REL_DUPLICATE_OF: &str = "duplicate_of";
pub const REL_UPSTREAM_CAUSE: &str = "upstream_cause";
pub const REL_DOWNSTREAM_SYMPTOM: &str = "downstream_symptom";
pub const REL_RELATED: &str = "related";
pub const REL_SUPERSEDES: &str = "supersedes";

pub const RELATIONSHIPS: &[&str] = &[
    REL_SAME_FINDING,
    REL_DUPLICATE_OF,
    REL_UPSTREAM_CAUSE,
    REL_DOWNSTREAM_SYMPTOM,
    REL_RELATED,
    REL_SUPERSEDES,
];

/// Symmetric relationships: canonical endpoint ordering (left < right) is
/// enforced so `relate A B same-finding` and `relate B A same-finding` are the
/// same assertion.
pub const SYMMETRIC_RELATIONSHIPS: &[&str] = &[REL_SAME_FINDING, REL_RELATED];

/// Directional relationships: endpoints are preserved as asserted and cycles
/// are rejected.
pub const DIRECTIONAL_RELATIONSHIPS: &[&str] = &[
    REL_DUPLICATE_OF,
    REL_UPSTREAM_CAUSE,
    REL_DOWNSTREAM_SYMPTOM,
    REL_SUPERSEDES,
];

// ---------------------------------------------------------------------------
// Verification statuses (v0 vocabulary).
// ---------------------------------------------------------------------------

pub const VERIFY_ACCEPTED: &str = "accepted";
pub const VERIFY_REJECTED: &str = "rejected";
pub const VERIFY_ABSTAINED: &str = "abstained";
pub const VERIFY_INVALID: &str = "invalid";
pub const VERIFY_UNKNOWN: &str = "unknown";

pub const VERIFICATION_STATUSES: &[&str] = &[
    VERIFY_ACCEPTED,
    VERIFY_REJECTED,
    VERIFY_ABSTAINED,
    VERIFY_INVALID,
    VERIFY_UNKNOWN,
];

/// A verification status that constitutes acceptance evidence.
pub fn is_accepted(status: &str) -> bool {
    status == VERIFY_ACCEPTED
}

/// A verification status that positively fails the remediation (keeps it
/// open, undoes any prior `verified_fixed`).
pub fn is_failing(status: &str) -> bool {
    matches!(status, VERIFY_REJECTED | VERIFY_INVALID)
}

// ---------------------------------------------------------------------------
// Event payloads. Every payload carries `created_at`; the optional
// `idempotency_key` is bound when the invoking command supplied one.
// ---------------------------------------------------------------------------

/// `observation_claimed` — a session acquired a claim lease on an observation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimedPayload {
    pub claim_id: String,
    pub claimed_by: String,
    pub claim_session_id: String,
    pub lease_expires_at: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `observation_claim_heartbeat` — the claiming session extended its lease.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimHeartbeatPayload {
    pub claim_id: String,
    pub claimed_by: String,
    pub claim_session_id: String,
    pub lease_expires_at: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `observation_claim_released` — the claiming session released the lease.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimReleasedPayload {
    pub claim_id: String,
    pub released_by: String,
    pub release_session_id: String,
    pub release_reason: String,
    pub released_at: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `observation_claim_expired` — a lease lapsed without release; recorded when
/// another session acquires the observation afterwards.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimExpiredPayload {
    pub claim_id: String,
    pub expired_at: String,
    pub created_at: String,
}

/// `observation_reviewed` — the reviewer's adjudication statement for an
/// observation. Emitted alongside `observation_disposition_set` by the
/// disposition command; history/audit surface (the state-bearing event is the
/// disposition set).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReviewedPayload {
    pub disposition: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_observation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_json: Option<String>,
    pub reviewer: String,
    pub review_session_id: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `observation_disposition_set` — the state-bearing disposition event. The
/// reducer derives the current disposition from the latest valid event of
/// this type for an observation. `disposition_id` (the disposition record's
/// own record id) is bound so the untagged payload encoding can never be
/// conflated with the structurally similar `observation_reviewed` statement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DispositionSetPayload {
    pub disposition_id: String,
    pub disposition: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_observation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_json: Option<String>,
    pub reviewer: String,
    pub review_session_id: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `observation_reopened` — a previous disposition was reopened (append-only;
/// the earlier disposition events remain). Clears the current disposition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReopenedPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    pub reviewer: String,
    pub review_session_id: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `observation_relationship_added` — an explicit reviewer assertion between
/// two observations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelationshipAddedPayload {
    pub relationship_id: String,
    pub left_observation_id: String,
    pub right_observation_id: String,
    pub relation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_json: Option<String>,
    pub reviewer: String,
    pub review_session_id: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `observation_relationship_retracted` — an explicit retraction of a prior
/// relationship assertion (no hard delete).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RelationshipRetractedPayload {
    pub relationship_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    pub reviewer: String,
    pub review_session_id: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `observation_promoted` — a confirmed observation was promoted to a finding.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PromotedPayload {
    pub finding_id: String,
    pub reviewer: String,
    pub review_session_id: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `remediation_task_attached` — owned work item linked to the observation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskAttachedPayload {
    pub task_id: String,
    pub reviewer: String,
    pub review_session_id: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `remediation_fix_attached` — a candidate fixing commit. A commit alone
/// never implies success (see the reducer).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FixAttachedPayload {
    pub commit_sha: String,
    pub repository_id: String,
    pub reviewer: String,
    pub review_session_id: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `remediation_verification_attached` — verification evidence for the
/// remediation. `accepted` is the only status that yields `verified_fixed`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerificationAttachedPayload {
    pub receipt_ref: String,
    pub status: String,
    pub reviewer: String,
    pub review_session_id: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `remediation_marked_handled` — explicit durable handling declaration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MarkedHandledPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    pub reviewer: String,
    pub review_session_id: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// `remediation_reopened` — a handled remediation was reopened (append-only).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RemediationReopenedPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    pub reviewer: String,
    pub review_session_id: String,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// The tagged remediation event envelope.
///
/// `RecordPayload` is an *untagged* enum; several remediation payloads are
/// structurally similar (e.g. `Reopened`/`MarkedHandled`/`RemediationReopened`
/// are identical shapes), so untagged deserialization could never
/// disambiguate them. Wrapping them in this internally-tagged envelope makes
/// the encoding unambiguous while leaving the outer `Observation` and
/// `Retraction` variants byte-compatible with existing stores.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "event_type", rename_all = "snake_case")]
pub enum RemediationEvent {
    Claimed(ClaimedPayload),
    ClaimHeartbeat(ClaimHeartbeatPayload),
    ClaimReleased(ClaimReleasedPayload),
    ClaimExpired(ClaimExpiredPayload),
    Reviewed(ReviewedPayload),
    DispositionSet(DispositionSetPayload),
    Reopened(ReopenedPayload),
    RelationshipAdded(RelationshipAddedPayload),
    RelationshipRetracted(RelationshipRetractedPayload),
    Promoted(PromotedPayload),
    TaskAttached(TaskAttachedPayload),
    FixAttached(FixAttachedPayload),
    VerificationAttached(VerificationAttachedPayload),
    MarkedHandled(MarkedHandledPayload),
    RemediationReopened(RemediationReopenedPayload),
}

/// The event payloads that carry an idempotency key.
pub fn payload_idempotency_key(payload: &crate::record::RecordPayload) -> Option<&str> {
    use crate::record::RecordPayload::*;
    match payload {
        Remediation(ev) => match ev {
            RemediationEvent::Claimed(p) => p.idempotency_key.as_deref(),
            RemediationEvent::ClaimHeartbeat(p) => p.idempotency_key.as_deref(),
            RemediationEvent::ClaimReleased(p) => p.idempotency_key.as_deref(),
            RemediationEvent::ClaimExpired(_) => None,
            RemediationEvent::Reviewed(p) => p.idempotency_key.as_deref(),
            RemediationEvent::DispositionSet(p) => p.idempotency_key.as_deref(),
            RemediationEvent::Reopened(p) => p.idempotency_key.as_deref(),
            RemediationEvent::RelationshipAdded(p) => p.idempotency_key.as_deref(),
            RemediationEvent::RelationshipRetracted(p) => p.idempotency_key.as_deref(),
            RemediationEvent::Promoted(p) => p.idempotency_key.as_deref(),
            RemediationEvent::TaskAttached(p) => p.idempotency_key.as_deref(),
            RemediationEvent::FixAttached(p) => p.idempotency_key.as_deref(),
            RemediationEvent::VerificationAttached(p) => p.idempotency_key.as_deref(),
            RemediationEvent::MarkedHandled(p) => p.idempotency_key.as_deref(),
            RemediationEvent::RemediationReopened(p) => p.idempotency_key.as_deref(),
        },
        _ => None,
    }
}

/// The `created_at` carried by any remediation event payload.
pub fn payload_created_at(payload: &crate::record::RecordPayload) -> Option<&str> {
    use crate::record::RecordPayload::*;
    match payload {
        Remediation(ev) => match ev {
            RemediationEvent::Claimed(p) => Some(&p.created_at),
            RemediationEvent::ClaimHeartbeat(p) => Some(&p.created_at),
            RemediationEvent::ClaimReleased(p) => Some(&p.created_at),
            RemediationEvent::ClaimExpired(p) => Some(&p.created_at),
            RemediationEvent::Reviewed(p) => Some(&p.created_at),
            RemediationEvent::DispositionSet(p) => Some(&p.created_at),
            RemediationEvent::Reopened(p) => Some(&p.created_at),
            RemediationEvent::RelationshipAdded(p) => Some(&p.created_at),
            RemediationEvent::RelationshipRetracted(p) => Some(&p.created_at),
            RemediationEvent::Promoted(p) => Some(&p.created_at),
            RemediationEvent::TaskAttached(p) => Some(&p.created_at),
            RemediationEvent::FixAttached(p) => Some(&p.created_at),
            RemediationEvent::VerificationAttached(p) => Some(&p.created_at),
            RemediationEvent::MarkedHandled(p) => Some(&p.created_at),
            RemediationEvent::RemediationReopened(p) => Some(&p.created_at),
        },
        _ => None,
    }
}

/// Outcome of appending (or replaying) one remediation event.
#[derive(Debug, Clone)]
pub struct AppendedEvent {
    pub record_id: String,
    pub local_sequence: i64,
    pub record_hash: String,
    /// True when the event was a same-key/same-payload replay (no new row).
    pub replayed: bool,
}

/// Append one remediation event to the global record stream inside `tx`.
///
/// Idempotency: when the payload carries an `idempotency_key`, a prior record
/// of the same type with the same key is looked up by JSON extraction. An
/// identical payload replays (returns the existing record, no new row); a
/// different payload is a typed conflict.
///
/// Failpoints: `remediation_after_record_alloc` fires after the sequence and
/// predecessor are allocated; `remediation_after_event_insert` after the
/// records row lands (crash injection tests prove both observable outcomes:
/// zero events or one complete event).
pub fn append_event(
    tx: &rusqlite::Transaction,
    store_id: &str,
    record_type: &str,
    entity_id: &str,
    payload: RecordPayload,
) -> anyhow::Result<AppendedEvent> {
    let idem = payload_idempotency_key(&payload);
    if let Some(ik) = idem {
        let mut stmt = tx.prepare(
            "SELECT record_id, local_sequence, record_hash, canonical_payload_json
             FROM records
             WHERE record_type = ?1 AND json_extract(canonical_payload_json, '$.idempotency_key') = ?2
             ORDER BY local_sequence ASC LIMIT 1",
        )?;
        let existing = stmt
            .query_row(rusqlite::params![record_type, ik], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .optional()?;
        if let Some((old_id, old_seq, old_hash, old_payload_json)) = existing {
            let old_payload: RecordPayload = serde_json::from_str(&old_payload_json)?;
            if old_payload == payload {
                return Ok(AppendedEvent {
                    record_id: old_id,
                    local_sequence: old_seq,
                    record_hash: old_hash,
                    replayed: true,
                });
            }
            anyhow::bail!(crate::error::SnagError::IdempotencyConflict(format!(
                "idempotency key {ik} already used with a different remediation payload"
            )));
        }
    }

    let local_sequence: i64 = tx.query_row(
        "SELECT COALESCE(MAX(local_sequence), 0) + 1 FROM records",
        [],
        |row| row.get(0),
    )?;
    let previous_record_hash: String = tx
        .query_row(
            "SELECT record_hash FROM records ORDER BY local_sequence DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| {
            "0000000000000000000000000000000000000000000000000000000000000000".to_string()
        });
    crate::failpoint::failpoint("remediation_after_record_alloc");

    let record_id = generate_id("rec");
    // The record's captured_at is the payload's created_at (the command
    // stamps both at the same instant), so the canonical kernel and the
    // payload never disagree about the event time.
    let captured_at = payload_created_at(&payload)
        .map(str::to_string)
        .unwrap_or_else(crate::remediation::identity::utc_now);
    let canonical_record = CanonicalRecordV1 {
        local_sequence: local_sequence as u64,
        record_id: record_id.clone(),
        record_type: record_type.to_string(),
        entity_id: entity_id.to_string(),
        captured_at: captured_at.clone(),
        payload,
    };
    let payload_json = serde_json::to_string(&canonical_record.payload)?;
    let record_hash = canonical_record.compute_hash(store_id, &previous_record_hash);

    tx.execute(
        "INSERT INTO records (local_sequence, record_id, record_type, entity_id, captured_at, canonical_payload_json, previous_record_hash, record_hash)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        rusqlite::params![
            local_sequence,
            &record_id,
            record_type,
            entity_id,
            &captured_at,
            &payload_json,
            &previous_record_hash,
            &record_hash,
        ],
    )?;
    crate::failpoint::failpoint("remediation_after_event_insert");

    Ok(AppendedEvent {
        record_id,
        local_sequence,
        record_hash,
        replayed: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::RecordPayload;

    #[test]
    fn vocabularies_are_stable_and_recognized() {
        assert_eq!(REMEDIATION_RECORD_TYPES.len(), 15);
        assert_eq!(DISPOSITIONS.len(), 7);
        assert_eq!(RELATIONSHIPS.len(), 6);
        assert_eq!(VERIFICATION_STATUSES.len(), 5);
        assert!(DISPOSITIONS.contains(&DISP_CONFIRMED));
        assert!(RELATIONSHIPS.contains(&REL_SUPERSEDES));
        assert!(is_accepted(VERIFY_ACCEPTED));
        assert!(!is_accepted(VERIFY_REJECTED));
        assert!(is_failing(VERIFY_REJECTED));
        assert!(is_failing(VERIFY_INVALID));
        assert!(!is_failing(VERIFY_ABSTAINED));
    }

    #[test]
    fn disposition_set_never_parses_as_reviewed() {
        // Regression: Reviewed and DispositionSet are structurally similar;
        // the untagged encoding MUST resolve a disposition-set payload to
        // DispositionSet (the superset variant) so rebuild/verify never
        // silently treat a state-bearing disposition as a history statement.
        let p =
            RecordPayload::Remediation(RemediationEvent::DispositionSet(DispositionSetPayload {
                disposition_id: "disp_1".into(),
                disposition: DISP_CONFIRMED.into(),
                target_observation_id: None,
                rationale: Some("reproduced".into()),
                evidence_json: None,
                reviewer: "reviewer_a".into(),
                review_session_id: "s1".into(),
                created_at: "2026-08-05T01:00:00Z".into(),
                idempotency_key: None,
            }));
        let json = serde_json::to_string(&p).unwrap();
        let back: RecordPayload = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            RecordPayload::Remediation(RemediationEvent::DispositionSet(_))
        ));

        let reviewed = RecordPayload::Remediation(RemediationEvent::Reviewed(ReviewedPayload {
            disposition: DISP_CONFIRMED.into(),
            target_observation_id: None,
            rationale: Some("reproduced".into()),
            evidence_json: None,
            reviewer: "reviewer_a".into(),
            review_session_id: "s1".into(),
            created_at: "2026-08-05T01:00:00Z".into(),
            idempotency_key: None,
        }));
        let json = serde_json::to_string(&reviewed).unwrap();
        let back: RecordPayload = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            RecordPayload::Remediation(RemediationEvent::Reviewed(_))
        ));
    }

    #[test]
    fn payloads_roundtrip_json() {
        let cases: Vec<RecordPayload> = vec![
            RecordPayload::Remediation(RemediationEvent::Claimed(ClaimedPayload {
                claim_id: "c1".into(),
                claimed_by: "reviewer_a".into(),
                claim_session_id: "s1".into(),
                lease_expires_at: "2026-08-05T01:00:00Z".into(),
                created_at: "2026-08-05T00:30:00Z".into(),
                idempotency_key: Some("ik1".into()),
            })),
            RecordPayload::Remediation(RemediationEvent::DispositionSet(DispositionSetPayload {
                disposition_id: "disp_1".into(),
                disposition: DISP_CONFIRMED.into(),
                target_observation_id: None,
                rationale: Some("reproduced".into()),
                evidence_json: None,
                reviewer: "reviewer_a".into(),
                review_session_id: "s1".into(),
                created_at: "2026-08-05T01:00:00Z".into(),
                idempotency_key: None,
            })),
            RecordPayload::Remediation(RemediationEvent::VerificationAttached(
                VerificationAttachedPayload {
                    receipt_ref: "receipt_1".into(),
                    status: VERIFY_ACCEPTED.into(),
                    reviewer: "reviewer_a".into(),
                    review_session_id: "s1".into(),
                    created_at: "2026-08-05T02:00:00Z".into(),
                    idempotency_key: None,
                },
            )),
        ];
        for p in cases {
            let json = serde_json::to_string(&p).unwrap();
            let back: RecordPayload = serde_json::from_str(&json).unwrap();
            assert_eq!(back, p);
        }
    }
}
