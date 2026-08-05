# Snag Audit — Post-Certification State

## Certification Status

**CERTIFIED v0.1.0** — accepted through the moat acceptance loop and merged
via PR #1 (merge commit `b8dda313700460ee6a55211e65a55eea378beb02`).

| Evidence | Value |
|---|---|
| Accepted revision | `0c54bf2` |
| Merge commit | `b8dda31` |
| PR | #1, merged |
| Tests | 59/59 pass |
| Moat | ACCEPTED |
| Release tag | `v0.1.0` |

The authoritative requirement digest (G20–G37, T1–T12) is
[.cert-reqs-digest.md](.cert-reqs-digest.md). The certified record kernel,
export/rebuild protocol, recovery chain, multirepo identity, idempotency,
migrations, and robustness matrix all shipped in PR #1.

## Post-Certification State

This file previously listed pre-certification gaps. Those gaps were closed by
the v0 certification work; the list below is the **current** audit state.

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
- **Robustness**: crash injection (T6), 32-writer concurrency (T5), git
  process-kill timeout, artifact constraints (symlink rejection, size caps).
- **Read purity**: distinct reader/writer/maintenance connection modes; list,
  show, context, export, verify, doctor proven non-mutating.
- **Verification**: `verify --full` recomputes the entire chain; `verify
  --quick` bounds the suffix with predecessor-hash equality.
- **Testing**: 59 tests across cli, git_identity, migration, concurrency,
  robustness suites; all gates green in clean sandbox.

### Known non-goals / documented limits (v0.1)
- No automatic runtime nudges (planned for Panopticon; agent confirmation is
  the quality gate while the corpus is small).
- No dashboard/UI (Panopticon v1 scope).
- `snag` itself is capture-only: coalescing, taxonomy, and materiality live in
  Panopticon, not here.

### Maintenance posture
- Snag source is **frozen** except for bugs discovered through its own use.
  Feature work belongs to Panopticon / the next design program.
- Every change ships through the moat acceptance loop (see
  [CLAUDE.md](CLAUDE.md)); never weaken a gate.
- CI runs the identical gate commands declared in `.moat.json`:
  `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`,
  `cargo test --all-targets --all-features --no-fail-fast`.

## Operational Checklist

Before declaring work done in this repo:

1. `cargo fmt --check`
2. `cargo clippy --all-targets --all-features -- -D warnings`
3. `cargo test --all-targets --all-features --no-fail-fast`
4. `snag verify` against the local store (if a store exists)
5. Contract the change, submit through moat, land only on ACCEPTED.
