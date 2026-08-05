use serde::{Deserialize, Serialize};
use crate::types::Observation;

/// Canonical record encoding version. Any change to the canonical structure or
/// the set of fields covered by the hash MUST bump this version so that records
/// produced before and after the change cannot be conflated.
pub const CANONICAL_ENCODING_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RetractionPayload {
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum RecordPayload {
    Observation(Observation),
    Retraction(RetractionPayload),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanonicalRecordV1 {
    pub local_sequence: u64,
    pub record_id: String,
    pub record_type: String,
    pub entity_id: String,
    pub captured_at: String,
    pub payload: RecordPayload,
}

impl CanonicalRecordV1 {
    pub fn compute_hash(&self, store_id: &str, previous_record_hash: &str) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(store_id.as_bytes());
        hasher.update(&CANONICAL_ENCODING_VERSION.to_le_bytes());
        hasher.update(&self.local_sequence.to_le_bytes());
        hasher.update(self.record_id.as_bytes());
        hasher.update(self.record_type.as_bytes());
        hasher.update(self.entity_id.as_bytes());
        hasher.update(self.captured_at.as_bytes());
        hasher.update(previous_record_hash.as_bytes());

        let payload_str = serde_json::to_string(&self.payload).expect("Deterministic serialization failed");
        hasher.update(payload_str.as_bytes());

        format!("blake3:{}", hasher.finalize().to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Observation;
    use std::collections::BTreeMap;

    fn sample_obs() -> Observation {
        Observation {
            schema_version: 1,
            observation_id: "obs_abc".to_string(),
            store_id: "store_x".to_string(),
            local_sequence: 1,
            idempotency_key: None,
            created_at: "2026-08-04T00:00:00Z".to_string(),
            source: crate::types::SourceInfo {
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
            confidence: None,
            sensitivity: crate::types::Sensitivity::Normal,
            labels: Some(BTreeMap::from([("k".to_string(), "v".to_string())])),
            context: crate::types::ContextInfo::default(),
            artifacts: vec![],
            affected_repository_ids: vec![],
        }
    }

    fn base() -> (CanonicalRecordV1, String) {
        let rec = CanonicalRecordV1 {
            local_sequence: 1,
            record_id: "obs_abc".to_string(),
            record_type: "observation_created".to_string(),
            entity_id: "obs_abc".to_string(),
            captured_at: "2026-08-04T00:00:00Z".to_string(),
            payload: RecordPayload::Observation(sample_obs()),
        };
        (rec, "store_x".to_string())
    }

    #[test]
    fn hash_is_deterministic() {
        let (a, s) = base();
        let (b, _) = base();
        assert_eq!(a.compute_hash(&s, "prev"), b.compute_hash(&s, "prev"));
    }

    #[test]
    fn hash_binds_payload_map_order_independently() {
        // BTreeMap guarantees determinism regardless of insertion order.
        let mut o1 = sample_obs();
        let mut labels_a = BTreeMap::new();
        labels_a.insert("a".to_string(), "1".to_string());
        labels_a.insert("b".to_string(), "2".to_string());
        o1.labels = Some(labels_a);
        let r1 = CanonicalRecordV1 {
            payload: RecordPayload::Observation(o1.clone()),
            ..base().0
        };
        let r2 = CanonicalRecordV1 {
            payload: RecordPayload::Observation(o1),
            ..base().0
        };
        assert_eq!(
            r1.compute_hash("store_x", "prev"),
            r2.compute_hash("store_x", "prev")
        );
    }

    macro_rules! tamper_test {
        ($name:ident, $mutate:expr) => {
            #[test]
            fn $name() {
                let (rec, store) = base();
                let original = rec.compute_hash(&store, "prev");
                let mut tampered = rec.clone();
                ($mutate)(&mut tampered);
                assert_ne!(original, tampered.compute_hash(&store, "prev"));
            }
        };
    }

    tamper_test!(tamper_local_sequence, |r: &mut CanonicalRecordV1| r.local_sequence = 2);
    tamper_test!(tamper_record_id, |r: &mut CanonicalRecordV1| r.record_id = "obs_different".to_string());
    tamper_test!(tamper_record_type, |r: &mut CanonicalRecordV1| r.record_type = "observation_retracted".to_string());
    tamper_test!(tamper_entity_id, |r: &mut CanonicalRecordV1| r.entity_id = "obs_other".to_string());
    tamper_test!(tamper_captured_at, |r: &mut CanonicalRecordV1| r.captured_at = "1999-01-01T00:00:00Z".to_string());

    #[test]
    fn tamper_store_id() {
        let (rec, _) = base();
        let original = rec.compute_hash("store_x", "prev");
        assert_ne!(original, rec.compute_hash("store_y", "prev"));
    }

    #[test]
    fn tamper_previous_hash() {
        let (rec, store) = base();
        let original = rec.compute_hash(&store, "prev");
        assert_ne!(original, rec.compute_hash(&store, "prev2"));
    }

    #[test]
    fn tamper_payload() {
        let (mut rec, store) = base();
        let original = rec.compute_hash(&store, "prev");
        match &mut rec.payload {
            RecordPayload::Observation(o) => o.title = "Changed title".to_string(),
            _ => unreachable!(),
        }
        assert_ne!(original, rec.compute_hash(&store, "prev"));
    }

    #[test]
    fn tamper_retraction_target() {
        // Changing the entity_id of a retraction must invalidate the hash.
        let rec = CanonicalRecordV1 {
            local_sequence: 2,
            record_id: "act_1".to_string(),
            record_type: "observation_retracted".to_string(),
            entity_id: "obs_target".to_string(),
            captured_at: "2026-08-04T00:00:01Z".to_string(),
            payload: RecordPayload::Retraction(RetractionPayload { reason: "r".to_string() }),
        };
        let original = rec.compute_hash("store_x", "prev");
        let mut tampered = rec.clone();
        tampered.entity_id = "obs_other_target".to_string();
        assert_ne!(original, tampered.compute_hash("store_x", "prev"));
    }

    #[test]
    fn payload_roundtrips() {
        let (rec, _) = base();
        let json = serde_json::to_string(&rec.payload).unwrap();
        let back: RecordPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, rec.payload);
    }
}

