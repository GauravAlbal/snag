//! Remediation identity resolution and lease duration parsing.
//!
//! Identity precedence (spec): explicit CLI arguments, then the remediation
//! context file (`SNAG_CONTEXT_FILE`, keys `reviewer_id` / `review_session_id`),
//! then the documented environment variables (`SNAG_REVIEWER_ID` /
//! `SNAG_REVIEW_SESSION_ID`), then a generated local session identifier.

use crate::types::generate_id;

/// The resolved reviewer + session pair for one remediation command.
#[derive(Debug, Clone)]
pub struct RemediationIdentity {
    pub reviewer: String,
    pub session_id: String,
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
/// 4. generated local identifiers (stable per invocation).
pub fn resolve_identity(
    cli_reviewer: Option<&str>,
    cli_session: Option<&str>,
) -> RemediationIdentity {
    let (reviewer, session_id) = if let Some(r) = cli_reviewer {
        (r.to_string(), cli_session.unwrap_or("cli").to_string())
    } else if let Some((r, s)) = from_context_file() {
        (r, s)
    } else {
        (
            std::env::var("SNAG_REVIEWER_ID").unwrap_or_else(|_| generate_id("reviewer")),
            std::env::var("SNAG_REVIEW_SESSION_ID").unwrap_or_else(|_| generate_id("session")),
        )
    };
    RemediationIdentity {
        reviewer,
        session_id,
    }
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
}
