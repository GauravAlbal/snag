# Snag Audit — Post-Certification State

Public engineering evidence for the v0.1.0 certification and the OSS-hardening
release that follows it.

## Certification Status

**CERTIFIED v0.1.0** — accepted through the project's mechanical acceptance
loop and merged via PR #1 (merge commit `b8dda313700460ee6a55211e65a55eea378beb02`).

| Evidence | Value |
|---|---|
| Accepted revision | `0c54bf2` |
| Merge commit | `b8dda31` |
| PR | #1, merged |
| Tests | 59/59 pass |
| Acceptance | ACCEPTED |
| Release tag | `v0.1.0` |

The v0 certification contract covered recovery/interchange (canonical record
encoding, export/rebuild protocol, safe non-destructive restore, complete
backup, independent backup verification), identity (real git common dir,
process-kill timeout, explicit precedence, affected-repo resolution, alias
ambiguity), input and idempotency (complete JSON intake, stable semantic
idempotency), migration (deterministic, collision-safe, forensic copy),
verification (full and quick), list behavior and read purity, plus the T1–T12
test matrix (CLI/input, idempotency, global records, real multirepo git
fixtures, 32-writer concurrency, crash injection, artifact adversarial tests,
export/rebuild round-trips, migration fixtures, restore failure injection,
read purity). The certified record kernel, export/rebuild protocol, recovery
chain, multirepo identity, idempotency, migrations, and robustness matrix all
shipped in PR #1.

## Post-Certification State

### Closed by certification (v0)
- **Canonical record kernel**: hash-chained, globally-sequenced records
  (`previous_record_hash` binding); tamper tests cover every bound field.
- **Export/rebuild protocol**: deterministic JSONL export consumed by
  `snag rebuild --from-export`; byte-identical output for identical state.
- **Recovery chain**: `backup` → `verify --backup` → `restore` (non-destructive,
  forensic copy preserved) → `rebuild` from export.
- **Multirepo identity**: real git common dir, worktree/checkout IDs, explicit
  precedence, affected-repo resolution with typed failures.
- **Idempotency**: stable semantic digest; same key + same digest replays,
  same key + different digest conflicts.
- **Migrations**: v1→v2 deterministic and collision-safe, transactional.
- **Robustness**: crash injection, 32-writer concurrency, git process-kill
  timeout, artifact constraints (symlink rejection, size caps).
- **Read purity**: distinct reader/writer/maintenance connection modes; list,
  show, context, export, verify, doctor proven non-mutating.
- **Verification**: `verify --full` recomputes the entire chain; `verify
  --quick` bounds the suffix with predecessor-hash equality.
- **Testing**: 59 tests across cli, git_identity, migration, concurrency,
  robustness suites; all gates green in clean sandbox.

### Closed by the OSS-hardening release (Unreleased)
- **CLI contract fixes** (both found by self-dogfood): `report --json` output
  mode with a bare title; structured flags on the fast path.
- **Public context protocol**: `schema_version` enforced on `SNAG_CONTEXT_FILE`
  documents; versioned `snag context --format json` envelope.
- **`snag doctor`**: prints database/objects/backups paths, effective context
  source, and version — no more guessing where data lives.
- **Schema compatibility gate**: `tests/schema_compat.rs` validates the
  binary's real export/context output against the published JSON Schemas.
- **Test surface**: 71 tests (59 certified + 12 new), all three gates green.

### Known non-goals / documented limits (v0.1)
- No automatic runtime nudges; agent confirmation is the quality gate while
  the corpus is small.
- No dashboard or UI; Snag is capture-only. Coalescing, taxonomy, and
  materiality belong to downstream consumers of the export stream.
- No cloud, no telemetry, no account (see [SECURITY.md](SECURITY.md)).
- The SQLite schema is internal, not a public API (see
  [docs/STABILITY.md](docs/STABILITY.md)).

### Maintenance posture
- Snag source changes only for bugs discovered through its own use and for
  installation/context compatibility work; feature work belongs to the
  roadmap ([docs/ROADMAP.md](docs/ROADMAP.md)).
- Every change ships through the mechanical acceptance loop; never weaken a
  gate.
- CI runs the identical gate commands declared in `.github/workflows/ci.yml`:
  `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
  `cargo test --all-targets --all-features --no-fail-fast`, plus `cargo deny`
  and `cargo audit`.

## Operational Checklist

Before declaring work done in this repo:

1. `cargo fmt --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test --all-targets --all-features --no-fail-fast`
4. `snag verify` against the local store (if a store exists)
5. Contract the change, submit through acceptance, land only on ACCEPTED.
