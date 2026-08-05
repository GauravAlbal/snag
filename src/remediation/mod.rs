//! The remediation protocol: queue retrieval, claim leases, append-only
//! adjudication, relationships, and remediation lineage over the global
//! append-only record stream.
//!
//! Event types and the reducer live in `events` and `reducer`; the command
//! handlers are added by the queue/claims, dispositions/relationships, and
//! lineage commits.

pub mod events;
pub mod reducer;
