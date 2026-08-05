//! Deterministic remediation state derivation.
//!
//! The reducer is a PURE function of the record stream: given the ordered
//! remediation events for one observation, it derives the current state. It
//! never reads the materialized tables. That makes it usable in three places
//! that must agree:
//!
//! 1. incremental updates (each remediation command recomputes the affected
//!    observation's state after appending its event, inside the same tx);
//! 2. rebuild (`snag rebuild --from-export`) reconstructs the materialized
//!    `observation_review_state` table purely from the replayed stream;
//! 3. `snag verify --full` replays the stream and cross-checks the
//!    materialized rows against the reduction.
//!
//! Materialized tables are projections, never independent authority.

use crate::record::RecordPayload;
use crate::remediation::events::*;
use std::collections::BTreeMap;

/// Current derived state vocabulary (subset of the spec's suggested list;
/// `adjudicated` is subsumed by the concrete disposition-derived states).
pub const STATE_UNREVIEWED: &str = "unreviewed";
pub const STATE_CLAIMED: &str = "claimed";
pub const STATE_CONFIRMED: &str = "confirmed";
pub const STATE_NEGATIVE_DISPOSITION: &str = "negative_disposition";
pub const STATE_PROMOTED: &str = "promoted";
pub const STATE_REMEDIATION_IN_PROGRESS: &str = "remediation_in_progress";
pub const STATE_CANDIDATE_FIX: &str = "candidate_fix";
pub const STATE_VERIFIED_FIXED: &str = "verified_fixed";
pub const STATE_DEFERRED: &str = "deferred";
pub const STATE_REOPENED: &str = "reopened";

/// A commit link derived from `remediation_fix_attached` events.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CommitLink {
    pub commit_sha: String,
    pub repository_id: String,
}

/// A verification receipt derived from `remediation_verification_attached`
/// events.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VerificationReceipt {
    pub receipt_ref: String,
    pub status: String,
}

/// The active claim derived from claim-lifecycle events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveClaim {
    pub claim_id: String,
    pub claimed_by: String,
    pub claim_session_id: String,
    pub lease_expires_at: String,
}

/// The reduced state of one observation. Immutable output of the reducer.
#[derive(Debug, Clone, PartialEq)]
pub struct ReducedObservation {
    pub observation_id: String,
    pub state: String,
    pub disposition: Option<String>,
    pub disposition_target: Option<String>,
    pub handled: bool,
    pub active_claim: Option<ActiveClaim>,
    pub promoted_finding_id: Option<String>,
    pub task_ids: Vec<String>,
    pub commits: Vec<CommitLink>,
    pub verification_receipts: Vec<VerificationReceipt>,
    pub latest_verification_status: Option<String>,
    /// Sequence of the last remediation event applied to this observation.
    pub last_event_sequence: i64,
}

/// One event in the stream, as the reducer consumes it.
#[derive(Debug, Clone)]
pub struct ReducedEvent<'a> {
    pub record_type: &'a str,
    pub payload: &'a RecordPayload,
    pub local_sequence: i64,
}

/// The reducer's mutable working state (per observation).
#[derive(Debug, Clone, Default)]
struct Work {
    state: String,
    disposition: Option<String>,
    disposition_target: Option<String>,
    handled: bool,
    active_claim: Option<ActiveClaim>,
    promoted_finding_id: Option<String>,
    task_ids: Vec<String>,
    commits: Vec<CommitLink>,
    verification_receipts: Vec<VerificationReceipt>,
    latest_verification_status: Option<String>,
    observation_reopened: bool,
    remediation_reopened: bool,
    marked_handled: bool,
}

impl Work {
    fn snapshot(&self, observation_id: &str, last_event_sequence: i64) -> ReducedObservation {
        ReducedObservation {
            observation_id: observation_id.to_string(),
            state: self.state.clone(),
            disposition: self.disposition.clone(),
            disposition_target: self.disposition_target.clone(),
            handled: self.handled,
            active_claim: self.active_claim.clone(),
            promoted_finding_id: self.promoted_finding_id.clone(),
            task_ids: self.task_ids.clone(),
            commits: self.commits.clone(),
            verification_receipts: self.verification_receipts.clone(),
            latest_verification_status: self.latest_verification_status.clone(),
            last_event_sequence,
        }
    }
}

/// Re-derive `state` from the current working fields (the precedence table
/// from the spec, applied after every event).
fn derive_state(w: &mut Work) {
    if let Some(disp) = &w.disposition {
        match disp.as_str() {
            DISP_DEFERRED => {
                w.state = STATE_DEFERRED.to_string();
                w.handled = true;
                w.observation_reopened = false;
            }
            d if TERMINAL_NEGATIVE_DISPOSITIONS.contains(&d) => {
                w.state = STATE_NEGATIVE_DISPOSITION.to_string();
                w.handled = true;
                w.observation_reopened = false;
            }
            DISP_CONFIRMED => {
                // Remediation progression: accepted verification is the only
                // terminal; commits alone never imply success. A durable
                // mark-handled declaration survives the re-derivation.
                if w.latest_verification_status.as_deref() == Some(VERIFY_ACCEPTED) {
                    w.state = STATE_VERIFIED_FIXED.to_string();
                    w.handled = true;
                } else if !w.commits.is_empty() {
                    w.state = STATE_CANDIDATE_FIX.to_string();
                } else if !w.task_ids.is_empty() {
                    w.state = STATE_REMEDIATION_IN_PROGRESS.to_string();
                } else if w.promoted_finding_id.is_some() {
                    w.state = STATE_PROMOTED.to_string();
                } else {
                    w.state = STATE_CONFIRMED.to_string();
                }
                // confirmed alone is not handled; an explicit mark-handled
                // declaration or an accepted receipt flips the flag.
                w.handled = w.marked_handled
                    || w.latest_verification_status.as_deref() == Some(VERIFY_ACCEPTED);
                w.observation_reopened = false;
            }
            _ => {
                w.state = STATE_CONFIRMED.to_string();
            }
        }
        w.remediation_reopened = false;
    } else {
        // No disposition: claimed beats the reopened/unreviewed markers only
        // when a claim is actually active.
        if let Some(_claim) = &w.active_claim {
            w.state = STATE_CLAIMED.to_string();
        } else if w.observation_reopened {
            w.state = STATE_REOPENED.to_string();
        } else {
            w.state = STATE_UNREVIEWED.to_string();
        }
    }
}

/// Apply one event to the working state. `remediation_reopened` and
/// `observation_reopened` short-circuit to `reopened` until a subsequent
/// event re-derives the state.
fn apply_event(w: &mut Work, ev: &ReducedEvent) {
    use crate::record::RecordPayload::Remediation as R;
    use crate::remediation::events::RemediationEvent as E;
    match (ev.record_type, &ev.payload) {
        (RECORD_CLAIMED, R(E::Claimed(p))) => {
            w.active_claim = Some(ActiveClaim {
                claim_id: p.claim_id.clone(),
                claimed_by: p.claimed_by.clone(),
                claim_session_id: p.claim_session_id.clone(),
                lease_expires_at: p.lease_expires_at.clone(),
            });
            derive_state(w);
        }
        (RECORD_CLAIM_HEARTBEAT, R(E::ClaimHeartbeat(p))) => {
            if let Some(claim) = &mut w.active_claim
                && claim.claim_id == p.claim_id
            {
                claim.lease_expires_at = p.lease_expires_at.clone();
            }
            derive_state(w);
        }
        (RECORD_CLAIM_RELEASED, R(E::ClaimReleased(p))) => {
            if let Some(claim) = &w.active_claim
                && claim.claim_id == p.claim_id
            {
                w.active_claim = None;
            }
            derive_state(w);
        }
        (RECORD_CLAIM_EXPIRED, R(E::ClaimExpired(p))) => {
            if let Some(claim) = &w.active_claim
                && claim.claim_id == p.claim_id
            {
                w.active_claim = None;
            }
            derive_state(w);
        }
        (RECORD_REVIEWED, R(E::Reviewed(_))) => {
            // History/audit surface only; the state-bearing event follows.
        }
        (RECORD_DISPOSITION_SET, R(E::DispositionSet(p))) => {
            w.disposition = Some(p.disposition.clone());
            w.disposition_target = p.target_observation_id.clone();
            derive_state(w);
        }
        (RECORD_REOPENED, R(E::Reopened(_))) => {
            w.disposition = None;
            w.disposition_target = None;
            w.handled = false;
            w.marked_handled = false;
            w.observation_reopened = true;
            w.state = STATE_REOPENED.to_string();
        }
        (RECORD_RELATIONSHIP_ADDED, R(E::RelationshipAdded(_)))
        | (RECORD_RELATIONSHIP_RETRACTED, R(E::RelationshipRetracted(_))) => {
            // Relationships are cross-observation; per-observation state is
            // unaffected.
        }
        (RECORD_PROMOTED, R(E::Promoted(p))) => {
            w.promoted_finding_id = Some(p.finding_id.clone());
            w.remediation_reopened = false;
            derive_state(w);
        }
        (RECORD_TASK_ATTACHED, R(E::TaskAttached(p))) => {
            if !w.task_ids.contains(&p.task_id) {
                w.task_ids.push(p.task_id.clone());
            }
            w.remediation_reopened = false;
            derive_state(w);
        }
        (RECORD_FIX_ATTACHED, R(E::FixAttached(p))) => {
            let link = CommitLink {
                commit_sha: p.commit_sha.clone(),
                repository_id: p.repository_id.clone(),
            };
            if !w.commits.contains(&link) {
                w.commits.push(link);
            }
            w.remediation_reopened = false;
            derive_state(w);
        }
        (RECORD_VERIFICATION_ATTACHED, R(E::VerificationAttached(p))) => {
            let receipt = VerificationReceipt {
                receipt_ref: p.receipt_ref.clone(),
                status: p.status.clone(),
            };
            if !w.verification_receipts.contains(&receipt) {
                w.verification_receipts.push(receipt);
            }
            w.latest_verification_status = Some(p.status.clone());
            w.remediation_reopened = false;
            derive_state(w);
        }
        (RECORD_MARKED_HANDLED, R(E::MarkedHandled(_))) => {
            w.marked_handled = true;
            w.handled = true;
            w.remediation_reopened = false;
            derive_state(w);
        }
        (RECORD_REMEDIATION_REOPENED, R(E::RemediationReopened(_))) => {
            w.handled = false;
            w.marked_handled = false;
            w.remediation_reopened = true;
            w.state = STATE_REOPENED.to_string();
        }
        _ => {
            // Unknown record type or payload mismatch: ignore. Rebuild/verify
            // must never fail on a record they did not produce.
        }
    }
}

/// Reduce an ordered event slice for one observation. Pure: no I/O.
pub fn reduce_events(observation_id: &str, events: &[ReducedEvent]) -> ReducedObservation {
    let mut w = Work {
        state: STATE_UNREVIEWED.to_string(),
        ..Work::default()
    };
    let mut last_seq = 0_i64;
    for ev in events {
        last_seq = ev.local_sequence;
        apply_event(&mut w, ev);
    }
    w.snapshot(observation_id, last_seq)
}

/// Owned-event variant used by stream callers (`reduce_observation`,
/// `replay_all`): collects owned tuples, borrows them for the duration of the
/// pure reduction, and drops them after.
pub fn reduce_owned(
    observation_id: &str,
    events: Vec<(i64, String, RecordPayload)>,
) -> ReducedObservation {
    let borrowed: Vec<ReducedEvent> = events
        .iter()
        .map(|(seq, typ, payload)| ReducedEvent {
            record_type: typ.as_str(),
            payload,
            local_sequence: *seq,
        })
        .collect();
    reduce_events(observation_id, &borrowed)
}

/// Reduce every observation's remediation events from the record stream.
///
/// Single pass over `records` in sequence order; events are grouped per
/// `entity_id` so each observation's slice keeps stream order. Pure function
/// of the stream — rebuild and verify use this, never the live tables.
pub fn replay_all(
    conn: &rusqlite::Connection,
) -> anyhow::Result<BTreeMap<String, ReducedObservation>> {
    let mut stmt = conn.prepare(
        "SELECT local_sequence, record_type, entity_id, canonical_payload_json
         FROM records ORDER BY local_sequence ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;

    let mut grouped: BTreeMap<String, Vec<(i64, String, RecordPayload)>> = BTreeMap::new();
    for row in rows {
        let (seq, record_type, entity_id, payload_json) = row?;
        if !REMEDIATION_RECORD_TYPES.contains(&record_type.as_str()) {
            continue;
        }
        let payload: RecordPayload = serde_json::from_str(&payload_json)?;
        grouped
            .entry(entity_id)
            .or_default()
            .push((seq, record_type, payload));
    }

    let mut out = BTreeMap::new();
    for (entity_id, events) in grouped {
        out.insert(entity_id.clone(), reduce_owned(&entity_id, events));
    }
    Ok(out)
}

/// Reduce one observation's remediation events from the record stream
/// (incremental path: commands recompute the affected observation's state
/// after appending, inside the same transaction).
pub fn reduce_observation(
    conn: &rusqlite::Connection,
    observation_id: &str,
) -> anyhow::Result<ReducedObservation> {
    let mut stmt = conn.prepare(
        "SELECT local_sequence, record_type, canonical_payload_json
         FROM records
         WHERE entity_id = ?1 AND record_type IN (
             'observation_claimed','observation_claim_heartbeat','observation_claim_released',
             'observation_claim_expired','observation_reviewed','observation_disposition_set',
             'observation_reopened','observation_relationship_added','observation_relationship_retracted',
             'observation_promoted','remediation_task_attached','remediation_fix_attached',
             'remediation_verification_attached','remediation_marked_handled','remediation_reopened'
         )
         ORDER BY local_sequence ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![observation_id], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut events: Vec<(i64, String, RecordPayload)> = Vec::new();
    for row in rows {
        let (seq, typ, payload_json) = row?;
        events.push((seq, typ, serde_json::from_str(&payload_json)?));
    }
    Ok(reduce_owned(observation_id, events))
}

/// Upsert the materialized `observation_review_state` projection for one
/// reduced observation. Used by rebuild (replay) and by the incremental
/// command path (recompute-after-append, same transaction).
pub fn upsert_review_state(
    tx: &rusqlite::Transaction,
    r: &ReducedObservation,
) -> anyhow::Result<()> {
    tx.execute(
        "INSERT INTO observation_review_state (
            observation_id, state, disposition, handled, active_claim_id,
            active_claim_expires_at, promoted_finding_id, task_ids_json, commits_json,
            verification_receipts_json, latest_verification_status, updated_through_sequence
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
        ON CONFLICT(observation_id) DO UPDATE SET
            state = excluded.state,
            disposition = excluded.disposition,
            handled = excluded.handled,
            active_claim_id = excluded.active_claim_id,
            active_claim_expires_at = excluded.active_claim_expires_at,
            promoted_finding_id = excluded.promoted_finding_id,
            task_ids_json = excluded.task_ids_json,
            commits_json = excluded.commits_json,
            verification_receipts_json = excluded.verification_receipts_json,
            latest_verification_status = excluded.latest_verification_status,
            updated_through_sequence = excluded.updated_through_sequence",
        rusqlite::params![
            &r.observation_id,
            &r.state,
            &r.disposition,
            r.handled as i64,
            &r.active_claim.as_ref().map(|c| c.claim_id.clone()),
            &r.active_claim.as_ref().map(|c| c.lease_expires_at.clone()),
            &r.promoted_finding_id,
            &serde_json::to_string(&r.task_ids)?,
            &serde_json::to_string(&r.commits)?,
            &serde_json::to_string(&r.verification_receipts)?,
            &r.latest_verification_status,
            r.last_event_sequence,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> String {
        "2026-08-05T00:00:00Z".to_string()
    }

    fn ev<'a>(seq: i64, typ: &'a str, payload: &'a RecordPayload) -> ReducedEvent<'a> {
        ReducedEvent {
            record_type: typ,
            payload,
            local_sequence: seq,
        }
    }

    fn claimed(id: &str, by: &str, expires: &str) -> RecordPayload {
        RecordPayload::Remediation(RemediationEvent::Claimed(ClaimedPayload {
            claim_id: id.to_string(),
            claimed_by: by.to_string(),
            claim_session_id: format!("session_{by}"),
            lease_expires_at: expires.to_string(),
            created_at: now(),
            idempotency_key: None,
        }))
    }

    fn disposition(disp: &str, target: Option<&str>) -> RecordPayload {
        RecordPayload::Remediation(RemediationEvent::DispositionSet(DispositionSetPayload {
            disposition_id: format!("disp_{}", ulid::Ulid::generate()),
            disposition: disp.to_string(),
            target_observation_id: target.map(|t| t.to_string()),
            rationale: None,
            evidence_json: None,
            reviewer: "reviewer_a".to_string(),
            review_session_id: "session_a".to_string(),
            created_at: now(),
            idempotency_key: None,
        }))
    }

    fn task(tid: &str) -> RecordPayload {
        RecordPayload::Remediation(RemediationEvent::TaskAttached(TaskAttachedPayload {
            task_id: tid.to_string(),
            reviewer: "reviewer_a".to_string(),
            review_session_id: "session_a".to_string(),
            created_at: now(),
            idempotency_key: None,
        }))
    }

    fn fix(sha: &str, repo: &str) -> RecordPayload {
        RecordPayload::Remediation(RemediationEvent::FixAttached(FixAttachedPayload {
            commit_sha: sha.to_string(),
            repository_id: repo.to_string(),
            reviewer: "reviewer_a".to_string(),
            review_session_id: "session_a".to_string(),
            created_at: now(),
            idempotency_key: None,
        }))
    }

    fn verify(status: &str, receipt: &str) -> RecordPayload {
        RecordPayload::Remediation(RemediationEvent::VerificationAttached(
            VerificationAttachedPayload {
                receipt_ref: receipt.to_string(),
                status: status.to_string(),
                reviewer: "reviewer_a".to_string(),
                review_session_id: "session_a".to_string(),
                created_at: now(),
                idempotency_key: None,
            },
        ))
    }

    fn reduce(events: &[ReducedEvent]) -> ReducedObservation {
        reduce_events("obs_1", events)
    }

    #[test]
    fn unreviewed_when_no_events() {
        let r = reduce(&[]);
        assert_eq!(r.state, STATE_UNREVIEWED);
        assert_eq!(r.disposition, None);
        assert!(!r.handled);
        assert_eq!(r.active_claim, None);
    }

    #[test]
    fn claim_marks_claimed_until_released() {
        let r = reduce(&[
            ev(
                1,
                RECORD_CLAIMED,
                &claimed("c1", "reviewer_a", "2026-08-05T01:00:00Z"),
            ),
            ev(
                2,
                RECORD_CLAIM_RELEASED,
                &RecordPayload::Remediation(RemediationEvent::ClaimReleased(
                    ClaimReleasedPayload {
                        claim_id: "c1".to_string(),
                        released_by: "reviewer_a".to_string(),
                        release_session_id: "session_reviewer_a".to_string(),
                        release_reason: "done".to_string(),
                        released_at: now(),
                        created_at: now(),
                        idempotency_key: None,
                    },
                )),
            ),
        ]);
        assert_eq!(r.state, STATE_UNREVIEWED);
        assert_eq!(r.active_claim, None);
    }

    #[test]
    fn confirmed_task_commit_accepted_is_verified() {
        let r = reduce(&[
            ev(
                1,
                RECORD_DISPOSITION_SET,
                &disposition(DISP_CONFIRMED, None),
            ),
            ev(2, RECORD_TASK_ATTACHED, &task("task_1")),
            ev(3, RECORD_FIX_ATTACHED, &fix("abc123", "repo_1")),
            ev(
                4,
                RECORD_VERIFICATION_ATTACHED,
                &verify(VERIFY_ACCEPTED, "receipt_1"),
            ),
        ]);
        assert_eq!(r.state, STATE_VERIFIED_FIXED);
        assert!(r.handled);
        assert_eq!(
            r.latest_verification_status.as_deref(),
            Some(VERIFY_ACCEPTED)
        );
    }

    #[test]
    fn commit_alone_never_verifies() {
        let r = reduce(&[
            ev(
                1,
                RECORD_DISPOSITION_SET,
                &disposition(DISP_CONFIRMED, None),
            ),
            ev(2, RECORD_FIX_ATTACHED, &fix("abc123", "repo_1")),
        ]);
        assert_eq!(r.state, STATE_CANDIDATE_FIX);
        assert!(!r.handled);
    }

    #[test]
    fn rejected_verification_keeps_remediation_open() {
        let r = reduce(&[
            ev(
                1,
                RECORD_DISPOSITION_SET,
                &disposition(DISP_CONFIRMED, None),
            ),
            ev(2, RECORD_TASK_ATTACHED, &task("task_1")),
            ev(
                3,
                RECORD_VERIFICATION_ATTACHED,
                &verify(VERIFY_REJECTED, "receipt_1"),
            ),
        ]);
        assert_eq!(r.state, STATE_REMEDIATION_IN_PROGRESS);
        assert!(!r.handled);
    }

    #[test]
    fn later_rejected_receipt_undoes_verified_fixed() {
        let r = reduce(&[
            ev(
                1,
                RECORD_DISPOSITION_SET,
                &disposition(DISP_CONFIRMED, None),
            ),
            ev(2, RECORD_FIX_ATTACHED, &fix("abc123", "repo_1")),
            ev(
                3,
                RECORD_VERIFICATION_ATTACHED,
                &verify(VERIFY_ACCEPTED, "receipt_1"),
            ),
            ev(
                4,
                RECORD_VERIFICATION_ATTACHED,
                &verify(VERIFY_REJECTED, "receipt_2"),
            ),
        ]);
        // Latest valid event wins deterministically: the newer rejected
        // receipt leaves remediation open at candidate_fix (commit present).
        assert_eq!(r.state, STATE_CANDIDATE_FIX);
        assert!(!r.handled);
        assert_eq!(
            r.latest_verification_status.as_deref(),
            Some(VERIFY_REJECTED)
        );
    }

    #[test]
    fn negative_dispositions_are_handled_without_patch() {
        for disp in [
            DISP_DUPLICATE,
            DISP_EXPECTED_BEHAVIOR,
            DISP_ENVIRONMENTAL,
            DISP_INSUFFICIENT_EVIDENCE,
            DISP_SUPERSEDED,
        ] {
            let r = reduce(&[ev(1, RECORD_DISPOSITION_SET, &disposition(disp, None))]);
            assert_eq!(r.state, STATE_NEGATIVE_DISPOSITION, "{disp}");
            assert!(r.handled, "{disp}");
        }
        let r = reduce(&[ev(
            1,
            RECORD_DISPOSITION_SET,
            &disposition(DISP_DEFERRED, None),
        )]);
        assert_eq!(r.state, STATE_DEFERRED);
        assert!(r.handled);
    }

    #[test]
    fn observation_reopen_clears_disposition() {
        let r = reduce(&[
            ev(
                1,
                RECORD_DISPOSITION_SET,
                &disposition(DISP_DUPLICATE, Some("obs_2")),
            ),
            ev(
                2,
                RECORD_REOPENED,
                &RecordPayload::Remediation(RemediationEvent::Reopened(ReopenedPayload {
                    rationale: Some("was not a duplicate".to_string()),
                    reviewer: "reviewer_a".to_string(),
                    review_session_id: "session_a".to_string(),
                    created_at: now(),
                    idempotency_key: None,
                })),
            ),
        ]);
        assert_eq!(r.state, STATE_REOPENED);
        assert_eq!(r.disposition, None);
        assert!(!r.handled);
    }

    #[test]
    fn remediation_reopen_unhandles_then_new_evidence_reapplies() {
        let r = reduce(&[
            ev(
                1,
                RECORD_DISPOSITION_SET,
                &disposition(DISP_CONFIRMED, None),
            ),
            ev(2, RECORD_FIX_ATTACHED, &fix("abc123", "repo_1")),
            ev(
                3,
                RECORD_VERIFICATION_ATTACHED,
                &verify(VERIFY_ACCEPTED, "receipt_1"),
            ),
            ev(
                4,
                RECORD_REMEDIATION_REOPENED,
                &RecordPayload::Remediation(RemediationEvent::RemediationReopened(
                    RemediationReopenedPayload {
                        rationale: Some("regression reported".to_string()),
                        reviewer: "reviewer_a".to_string(),
                        review_session_id: "session_a".to_string(),
                        created_at: now(),
                        idempotency_key: None,
                    },
                )),
            ),
        ]);
        assert_eq!(r.state, STATE_REOPENED);
        assert!(!r.handled);
        // A new accepted receipt re-derives to verified_fixed.
        let r2 = reduce(&[
            ev(
                1,
                RECORD_DISPOSITION_SET,
                &disposition(DISP_CONFIRMED, None),
            ),
            ev(2, RECORD_FIX_ATTACHED, &fix("abc123", "repo_1")),
            ev(
                3,
                RECORD_VERIFICATION_ATTACHED,
                &verify(VERIFY_ACCEPTED, "receipt_1"),
            ),
            ev(
                4,
                RECORD_REMEDIATION_REOPENED,
                &RecordPayload::Remediation(RemediationEvent::RemediationReopened(
                    RemediationReopenedPayload {
                        rationale: Some("regression reported".to_string()),
                        reviewer: "reviewer_a".to_string(),
                        review_session_id: "session_a".to_string(),
                        created_at: now(),
                        idempotency_key: None,
                    },
                )),
            ),
            ev(
                5,
                RECORD_VERIFICATION_ATTACHED,
                &verify(VERIFY_ACCEPTED, "receipt_2"),
            ),
        ]);
        assert_eq!(r2.state, STATE_VERIFIED_FIXED);
        assert!(r2.handled);
    }

    #[test]
    fn promoted_confirmed_reaches_promoted_state() {
        let r = reduce(&[
            ev(
                1,
                RECORD_DISPOSITION_SET,
                &disposition(DISP_CONFIRMED, None),
            ),
            ev(
                2,
                RECORD_PROMOTED,
                &RecordPayload::Remediation(RemediationEvent::Promoted(PromotedPayload {
                    finding_id: "finding_1".to_string(),
                    reviewer: "reviewer_a".to_string(),
                    review_session_id: "session_a".to_string(),
                    created_at: now(),
                    idempotency_key: None,
                })),
            ),
        ]);
        assert_eq!(r.state, STATE_PROMOTED);
        assert_eq!(r.promoted_finding_id.as_deref(), Some("finding_1"));
        assert!(!r.handled);
    }

    #[test]
    fn replay_all_groups_by_observation_in_stream_order() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE records (
                local_sequence INTEGER PRIMARY KEY,
                record_type TEXT NOT NULL,
                entity_id TEXT NOT NULL,
                canonical_payload_json TEXT NOT NULL
            )",
        )
        .unwrap();
        let payloads = [
            (
                1,
                RECORD_CLAIMED,
                "obs_a",
                &claimed("c1", "reviewer_a", "2026-08-05T01:00:00Z"),
            ),
            (
                2,
                RECORD_DISPOSITION_SET,
                "obs_b",
                &disposition(DISP_CONFIRMED, None),
            ),
            (
                3,
                RECORD_DISPOSITION_SET,
                "obs_a",
                &disposition(DISP_DUPLICATE, Some("obs_b")),
            ),
        ];
        for (seq, typ, entity, payload) in payloads {
            conn.execute(
                "INSERT INTO records (local_sequence, record_type, entity_id, canonical_payload_json)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![seq, typ, entity, serde_json::to_string(payload).unwrap()],
            )
            .unwrap();
        }
        let all = replay_all(&conn).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all["obs_a"].state, STATE_NEGATIVE_DISPOSITION);
        assert_eq!(all["obs_a"].last_event_sequence, 3);
        assert_eq!(all["obs_b"].state, STATE_CONFIRMED);
        assert_eq!(all["obs_b"].last_event_sequence, 2);
    }
}
