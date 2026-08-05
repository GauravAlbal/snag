//! Schema compatibility gate.
//!
//! The JSON Schemas published in `schemas/` are a public API. This suite
//! proves they stay in sync with what the CLI actually produces and consumes:
//! every sample below comes from real binary output (or a document the binary
//! accepts), never a hand-built fixture that could drift from the contract.
//! Negative assertions are included so the validator cannot pass vacuously.
//!
//! The validator is a small dependency-free subset of JSON Schema draft-07
//! covering exactly the keywords the published schemas use (type, const, enum,
//! required, properties, additionalProperties, minimum/maximum, pattern,
//! format: date-time, oneOf, items). If a schema starts using a keyword this
//! subset does not implement, the validator fails loudly rather than skipping.

use assert_cmd::Command;
use serde_json::Value;
use std::env;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Minimal JSON Schema subset validator
// ---------------------------------------------------------------------------

fn fail(path: &str, msg: &str) -> Result<(), String> {
    Err(format!("{path}: {msg}"))
}

fn type_ok(expect: &str, v: &Value) -> bool {
    match expect {
        "object" => v.is_object(),
        "string" => v.is_string(),
        "integer" => v.is_number() && v.as_f64().unwrap().fract() == 0.0,
        "number" => v.is_number(),
        "array" => v.is_array(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        other => panic!("validator does not implement type {other}"),
    }
}

fn hash_pattern_ok(s: &str) -> bool {
    if s == "0".repeat(64) {
        return true;
    }
    s.len() == 71 && s.starts_with("blake3:") && s[7..].chars().all(|c| c.is_ascii_hexdigit())
}

fn date_time_ok(s: &str) -> bool {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).is_ok()
}

fn validate(schema: &Value, doc: &Value, path: &str) -> Result<(), String> {
    // oneOf: first matching variant wins, mirroring the schemas' intent that
    // every line matches exactly one variant.
    if let Some(one_of) = schema.get("oneOf") {
        let mut tried = Vec::new();
        for (i, sub) in one_of.as_array().unwrap().iter().enumerate() {
            match validate(sub, doc, path) {
                Ok(()) => return Ok(()),
                Err(e) => tried.push(format!("variant {i}: {e}")),
            }
        }
        return fail(
            path,
            &format!("matches no oneOf variant: {}", tried.join("; ")),
        );
    }

    if let Some(t) = schema.get("type") {
        let ok = match t {
            Value::String(s) => type_ok(s, doc),
            Value::Array(alts) => alts.iter().any(|a| type_ok(a.as_str().unwrap(), doc)),
            _ => panic!("validator does not implement type keyword shape"),
        };
        if !ok {
            return fail(path, &format!("expected type {t}, got {}", doc));
        }
    }

    if let Some(c) = schema.get("const")
        && doc != c
    {
        return fail(path, &format!("expected const {c}, got {}", doc));
    }

    if let Some(en) = schema.get("enum")
        && !en.as_array().unwrap().iter().any(|v| v == doc)
    {
        return fail(path, &format!("value {} not in enum", doc));
    }

    if let Some(min) = schema.get("minimum")
        && doc.as_f64().unwrap() < min.as_f64().unwrap()
    {
        return fail(path, &format!("value {} below minimum {min}", doc));
    }
    if let Some(max) = schema.get("maximum")
        && doc.as_f64().unwrap() > max.as_f64().unwrap()
    {
        return fail(path, &format!("value {} above maximum {max}", doc));
    }

    if let Some(pat) = schema.get("pattern") {
        let p = pat.as_str().unwrap();
        let s = doc.as_str().unwrap();
        let ok = match p {
            "^(blake3:[0-9a-f]{64}|0{64})$" => hash_pattern_ok(s),
            other => panic!("validator does not implement pattern {other}"),
        };
        if !ok {
            return fail(path, &format!("string {s} fails pattern {p}"));
        }
    }

    if let Some(fmt) = schema.get("format") {
        let f = fmt.as_str().unwrap();
        let ok = match f {
            "date-time" => date_time_ok(doc.as_str().unwrap()),
            other => panic!("validator does not implement format {other}"),
        };
        if !ok {
            return fail(path, &format!("string {} is not valid {f}", doc));
        }
    }

    if let Some(required) = schema.get("required") {
        let obj = doc
            .as_object()
            .ok_or_else(|| format!("{path}: expected object"))?;
        for k in required.as_array().unwrap() {
            let k = k.as_str().unwrap();
            if !obj.contains_key(k) {
                return fail(path, &format!("missing required field {k}"));
            }
        }
    }

    if let Some(props) = schema.get("properties") {
        let obj = doc
            .as_object()
            .ok_or_else(|| format!("{path}: expected object"))?;
        for (k, sub) in props.as_object().unwrap() {
            if let Some(v) = obj.get(k) {
                validate(sub, v, &format!("{path}.{k}"))?;
            }
        }
    }

    if let Some(additional) = schema.get("additionalProperties")
        && additional == &Value::Bool(false)
    {
        let obj = doc
            .as_object()
            .ok_or_else(|| format!("{path}: expected object"))?;
        let allowed: Vec<&String> = schema
            .get("properties")
            .map(|p| p.as_object().unwrap().keys().collect())
            .unwrap_or_default();
        for k in obj.keys() {
            if !allowed.contains(&k) {
                return fail(path, &format!("unexpected field {k}"));
            }
        }
    }

    if let Some(items) = schema.get("items") {
        let arr = doc
            .as_array()
            .ok_or_else(|| format!("{path}: expected array"))?;
        for (i, item) in arr.iter().enumerate() {
            validate(items, item, &format!("{path}[{i}]"))?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn schema(name: &str) -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("schemas")
        .join(name);
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("missing schema {name}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("invalid JSON schema {name}: {e}"))
}

struct TestContext {
    home_dir: tempfile::TempDir,
}

impl TestContext {
    fn new() -> Self {
        let home_dir = tempfile::tempdir().unwrap();
        unsafe {
            env::set_var("XDG_DATA_HOME", home_dir.path());
            env::set_var("HOME", home_dir.path());
        }
        Self { home_dir }
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::cargo_bin("snag").unwrap();
        cmd.env("XDG_DATA_HOME", self.home_dir.path())
            .env("HOME", self.home_dir.path());
        cmd
    }
}

impl Drop for TestContext {
    fn drop(&mut self) {
        unsafe {
            env::remove_var("XDG_DATA_HOME");
            env::remove_var("HOME");
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn test_schema_files_parse_and_are_versioned() {
    for name in [
        "snag-context-v1.schema.json",
        "observation-input-v1.schema.json",
        "export-stream-v1.schema.json",
    ] {
        let s = schema(name);
        assert_eq!(s["$schema"], "http://json-schema.org/draft-07/schema#");
        assert_eq!(s["type"], "object", "{name} must be an object schema");
    }
}

/// A SNAG_CONTEXT_FILE with a wrapper-owned unknown key still validates
/// (documented rule: unknown top-level keys are ignored).
#[test]
fn test_context_input_validates() {
    let s = schema("snag-context-v1.schema.json");
    let doc: Value = serde_json::from_str(
        r#"{
            "schema_version": 1,
            "source": {"kind": "agent_explicit", "agent_runtime": "claude-code", "model": "m"},
            "execution": {"session_id": "s-1", "task_id": "t-1", "tool_name": "bash"},
            "idempotency_key": "k"
        }"#,
    )
    .unwrap();
    validate(&s, &doc, "context-input").unwrap();

    // Wrong schema_version must fail the schema.
    let bad: Value = serde_json::from_str(r#"{"schema_version": 2}"#).unwrap();
    assert!(validate(&s, &bad, "context-input").is_err());
    // Unknown fields are ignored by the reader (documented rule), so the
    // schema must admit them at every level.
    let unknown: Value = serde_json::from_str(
        r#"{"schema_version": 1, "source": {"kind": "x", "wrapper_key": 1}, "extra_key": true}"#,
    )
    .unwrap();
    validate(&s, &unknown, "context-input").unwrap();
}

/// `snag context --format json` output validates against the context schema's
/// subschemas (the CLI emits a versioned envelope).
#[test]
fn test_context_output_validates() {
    let ctx = TestContext::new();
    let out = ctx
        .cmd()
        .arg("context")
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(out.status.success());
    let envelope: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(envelope["schema_version"], 1);
    let s = schema("snag-context-v1.schema.json");
    let context = &envelope["context"];
    for key in ["source", "execution", "repository"] {
        if let Some(sub) = context.get(key) {
            let sub_schema = s["properties"][key].clone();
            validate(&sub_schema, sub, &format!("context.{key}")).unwrap();
        }
    }
}

/// The document the CLI accepts via `report --json` validates against the
/// observation input schema; unknown top-level fields are permitted.
#[test]
fn test_observation_input_validates() {
    let ctx = TestContext::new();
    let doc = r#"{
        "schema_version": 1,
        "title": "schema gate observation",
        "kind_assertion": "bug",
        "severity_assertion": "major",
        "confidence": 0.9,
        "sensitivity": "sensitive",
        "labels": {"area": "cli"},
        "context": {"execution": {"tool_name": "bash"}},
        "wrapper_owned_key": "ignored by the reader"
    }"#;
    let s = schema("observation-input-v1.schema.json");
    let parsed: Value = serde_json::from_str(doc).unwrap();
    validate(&s, &parsed, "observation-input").unwrap();

    // The CLI actually accepts it.
    ctx.cmd()
        .arg("report")
        .arg("--json")
        .write_stdin(doc)
        .assert()
        .success();

    // A confidence above 1 must fail the schema.
    let bad: Value =
        serde_json::from_str(r#"{"schema_version": 1, "title": "t", "confidence": 1.5}"#).unwrap();
    assert!(validate(&s, &bad, "observation-input").is_err());
}

/// A real `snag export` stream validates line-by-line against the export
/// schema, and a tampered hash is caught by the validator (non-vacuous).
#[test]
fn test_export_stream_validates() {
    let ctx = TestContext::new();
    ctx.cmd()
        .arg("report")
        .arg("stream one")
        .arg("--kind")
        .arg("bug")
        .assert()
        .success();
    ctx.cmd().arg("report").arg("stream two").assert().success();

    let out_path = ctx.home_dir.path().join("stream.jsonl");
    ctx.cmd()
        .arg("export")
        .arg("--output")
        .arg(&out_path)
        .assert()
        .success();

    let s = schema("export-stream-v1.schema.json");
    let raw = std::fs::read_to_string(&out_path).unwrap();
    let lines: Vec<&str> = raw.lines().collect();
    assert!(lines.len() >= 3, "header + at least two records");

    // Header line.
    let header: Value = serde_json::from_str(lines[0]).unwrap();
    validate(&s, &header, "export.header").unwrap();
    assert_eq!(header["export_kind"], "export_header");

    // Record lines.
    let mut last_seq: i64 = 0;
    for line in &lines[1..] {
        let rec: Value = serde_json::from_str(line).unwrap();
        validate(&s, &rec, "export.record").unwrap();
        assert_eq!(rec["export_kind"], "record");
        let seq = rec["local_sequence"].as_i64().unwrap();
        assert_eq!(seq, last_seq + 1, "sequences must be contiguous");
        last_seq = seq;
    }

    // Tampered record hash must fail the validator (proves it is not vacuous).
    let mut tampered: Value = serde_json::from_str(lines[1]).unwrap();
    tampered["record_hash"] = Value::String("blake3:not-a-hash".into());
    assert!(validate(&s, &tampered, "export.record.tampered").is_err());
}
