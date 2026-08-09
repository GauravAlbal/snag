//! Remediation identity resolution and lease duration parsing.
//!
//! Identity precedence (spec): explicit CLI arguments, then the remediation
//! context file (`SNAG_CONTEXT_FILE`, keys `reviewer_id` / `review_session_id`),
//! then the documented environment variables (`SNAG_REVIEWER_ID` /
//! `SNAG_REVIEW_SESSION_ID`), then a generated local session identifier.

use crate::types::generate_id;
use serde::{Deserialize, Serialize};
use std::io::Write;

/// The resolved reviewer + session pair for one remediation command.
#[derive(Debug, Clone)]
pub struct RemediationIdentity {
    pub reviewer: String,
    pub session_id: String,
}

/// Persisted per-store remediation session (the default identity when nothing
/// external is supplied).
///
/// Without persistence, every CLI invocation mints a fresh `session_<ulid>`
/// and the multi-command flow (claim -> … -> release) can never release its
/// own lease. The file lives in the store's data dir so each store gets its
/// own session; concurrent remediation lanes MUST set `SNAG_REVIEW_SESSION_ID`
/// (or `--session-id`) to keep their claims isolated.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionFile {
    reviewer_id: String,
    session_id: String,
}

fn session_file_path() -> Option<std::path::PathBuf> {
    crate::store::Store::paths()
        .ok()
        .map(|(data_dir, _)| data_dir.join("review_session.json"))
}
fn read_session_file() -> Option<(String, String)> {
    let path = session_file_path()?;
    if path.exists() {
        crate::store::ensure_private_file(&path)
            .expect("failed to secure remediation session file");
    }
    let content = std::fs::read_to_string(path).ok()?;
    let parsed: SessionFile = serde_json::from_str(&content).ok()?;
    Some((parsed.reviewer_id, parsed.session_id))
}

fn write_session_file(reviewer: &str, session: &str) {
    let Some(path) = session_file_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        crate::store::ensure_private_dir(parent)
            .expect("failed to secure remediation session directory");
    }
    let payload = SessionFile {
        reviewer_id: reviewer.to_string(),
        session_id: session.to_string(),
    };
    let bytes = serde_json::to_vec(&payload).expect("failed to serialize remediation session");
    // Atomic publish: create a private temp sibling, sync it, then rename.
    let tmp = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .expect("failed to create remediation session temporary file");
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .expect("failed to write remediation session temporary file");
    crate::store::ensure_private_file(&tmp)
        .expect("failed to secure remediation session temporary file");
    std::fs::rename(&tmp, &path).expect("failed to publish remediation session");
    crate::store::ensure_private_file(&path).expect("failed to secure remediation session file");
}

/// Read the remediation identity from `SNAG_CONTEXT_FILE`, if set.
fn from_context_file() -> Option<(String, String)> {
    let path = std::env::var("SNAG_CONTEXT_FILE").ok()?;
    let content = std::fs::read_to_string(path).ok()?;
    let parsed: serde_json::Value = serde_json::from_str(&content).ok()?;
    let reviewer = parsed.get("reviewer_id")?.as_str()?.to_string();
    let session = parsed.get("review_session_id")?.as_str()?.to_string();
    Some((reviewer, session))
}

/// Resolve reviewer + session identity for a remediation command.
///
/// 1. explicit CLI arguments;
/// 2. remediation context file;
/// 3. `SNAG_REVIEWER_ID` / `SNAG_REVIEW_SESSION_ID`;
/// 4. the store's persisted session file (stable across CLI invocations);
/// 5. generated identifiers, then persisted for the next invocation.
pub fn resolve_identity(
    cli_reviewer: Option<&str>,
    cli_session: Option<&str>,
) -> RemediationIdentity {
    if let Some(r) = cli_reviewer {
        // Reviewer given without a session: derive a stable per-store session
        // (never the literal "cli" shared by every caller).
        let session = cli_session
            .map(|s| s.to_string())
            .unwrap_or_else(|| stable_session(r));
        return RemediationIdentity {
            reviewer: r.to_string(),
            session_id: session,
        };
    }
    if let Some(s) = cli_session {
        // A session WITHOUT a reviewer must still be honored: claim-scoped
        // commands (release) validate against the claim's stored session, and
        // a multi-command remediation session passes --session-id on every
        // step. Before this branch the flag fell through to the context file /
        // env pair / session file and every `release --session-id <claim>`
        // failed with a ClaimConflict naming a freshly-minted session.
        let reviewer = std::env::var("SNAG_REVIEWER_ID")
            .ok()
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| "cli".to_string());
        return RemediationIdentity {
            reviewer,
            session_id: s.to_string(),
        };
    }
    if let Some((r, s)) = from_context_file() {
        return RemediationIdentity {
            reviewer: r,
            session_id: s,
        };
    }
    match (
        std::env::var("SNAG_REVIEWER_ID"),
        std::env::var("SNAG_REVIEW_SESSION_ID"),
    ) {
        (Ok(r), Ok(s)) if !r.is_empty() && !s.is_empty() => {
            return RemediationIdentity {
                reviewer: r,
                session_id: s,
            };
        }
        (Ok(r), _) if !r.is_empty() => {
            // Reviewer set, session not: derive a stable per-store session for
            // this reviewer so sequential commands share it.
            let session = stable_session(&r);
            return RemediationIdentity {
                reviewer: r,
                session_id: session,
            };
        }
        _ => {}
    }
    if let Some((r, s)) = read_session_file() {
        return RemediationIdentity {
            reviewer: r,
            session_id: s,
        };
    }
    let reviewer = generate_id("reviewer");
    let session_id = generate_id("session");
    write_session_file(&reviewer, &session_id);
    RemediationIdentity {
        reviewer,
        session_id,
    }
}

/// A deterministic session for a reviewer when only the reviewer is known:
/// stable across invocations on the same store, distinct across reviewers.
fn stable_session(reviewer: &str) -> String {
    let data_dir = crate::store::Store::paths()
        .ok()
        .map(|(d, _)| d.display().to_string())
        .unwrap_or_default();
    let mut h = blake3::Hasher::new();
    h.update(b"review-session-v1");
    h.update(data_dir.as_bytes());
    h.update(b"|");
    h.update(reviewer.as_bytes());
    format!("session_{}", &h.finalize().to_hex()[..20])
}

/// The default lease duration in seconds (from `SNAG_REVIEW_LEASE` or 30m).
pub fn default_lease_seconds() -> u64 {
    if let Ok(raw) = std::env::var("SNAG_REVIEW_LEASE")
        && let Ok(secs) = parse_duration(&raw)
    {
        return secs;
    }
    30 * 60
}

/// Parse a human duration (`45s`, `30m`, `2h`, `1d`) into seconds.
pub fn parse_duration(raw: &str) -> Result<u64, String> {
    let raw = raw.trim();
    let (num, unit) = raw
        .split_at_checked(raw.len().saturating_sub(1))
        .ok_or_else(|| format!("invalid duration: {raw}"))?;
    let n: u64 = num
        .parse()
        .map_err(|_| format!("invalid duration: {raw}"))?;
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return Err(format!("invalid duration unit: {raw}")),
    };
    Ok(n.saturating_mul(mult))
}

/// Format a UTC timestamp in the store's RFC3339 shape.
pub fn utc_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

/// Compute `now + lease_seconds` as an RFC3339 UTC timestamp.
pub fn lease_expiry(now: &str, lease_seconds: u64) -> String {
    let parsed = time::OffsetDateTime::parse(now, &time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    (parsed + time::Duration::seconds(lease_seconds as i64))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_durations() {
        assert_eq!(parse_duration("45s").unwrap(), 45);
        assert_eq!(parse_duration("30m").unwrap(), 1800);
        assert_eq!(parse_duration("2h").unwrap(), 7200);
        assert_eq!(parse_duration("1d").unwrap(), 86400);
        assert!(parse_duration("30x").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn expiry_is_lexicographically_later() {
        let now = "2026-08-05T00:00:00Z";
        let later = lease_expiry(now, 3600);
        assert!(later.as_str() > now);
        assert!(later.starts_with("2026-08-05T01:00:00"));
    }

    #[test]
    fn session_without_reviewer_is_honored() {
        // Regression (2026-08-05): `--session-id` alone fell through to the
        // context/env/session-file and every claim-scoped `release
        // --session-id <claim>` failed with a ClaimConflict naming a fresh
        // session. The session flag must bind the identity by itself.
        let identity = resolve_identity(None, Some("session_explicit"));
        assert_eq!(identity.session_id, "session_explicit");
        let pair = resolve_identity(Some("rev"), Some("session_pair"));
        assert_eq!(pair.reviewer, "rev");
        assert_eq!(pair.session_id, "session_pair");
    }
}
