# Snag v0 Certification Audit

## Current State & Identified Gaps

### CLI & Input (G1, G2, G9)
- **JSON Input (G1)**: `report --json` currently does not parse the incoming JSON or merge it with context. It needs full JSON schema validation.
- **Prose Input (G2)**: `--stdin` currently only extracts the title. A simple deterministic text parser is needed to extract headings like `Expected:`, `Observed:`.
- **List Filters (G9)**: `list` command currently ignores all filters.

### Core Data Model & Schema (G3, G4, G6, G18)
- **Global Record Stream (G4)**: Observations and actions currently have split sequences. We need a schema migration to merge them into a single `records` table with a global sequence and hash chain.
- **Idempotency (G3)**: Currently not wired up. Needs a lookup against `canonical_payload` equivalence.
- **Affected Repositories (G6)**: Currently not parsed or persisted in `observation_repositories`.
- **Read-Purity (G18)**: `Store::open()` currently always applies migrations. We need distinct `open_read_only`, `open_read_write`, `open_for_maintenance` modes.

### Context & Git (G5, G7, G8)
- **Identity (G5)**: `identity.rs` is a stub. Requires full resolution of `git_common_dir`, remote aliases, checkouts, and worktrees.
- **Context File (G7)**: `SNAG_CONTEXT_FILE` only reads `source`. Must read execution, repository, idempotency key, etc., following precedence rules.
- **Git Boundaries (G8)**: Git context collection has no timeout and can hang.

### Artifacts (G16)
- File size limits exist (50MiB), but no total report limit.
- Missing existence checking before blindly writing over objects.
- Symlinks need to be explicitly rejected.

### Backup, Restore, Export & Verify (G10-G15)
- **Export (G10)**: Current export lacks correct `export_kind` header fields, actions inclusion, and partial chain predecessor validation.
- **Backup (G11)**: Current backup writes a basic manifest but misses `objects-manifest.json` and strict validation.
- **Verify (G12, G13)**: `verify --full` needs to recalculate hashes, match artifact lengths and digests, and validate metadata consistently.
- **Restore & Rebuild (G14, G15)**: Neither `restore` nor `rebuild` exist. Both are required for certification.

### Error Handling (G17)
- `SnagError` exists but errors do not format into the required JSON envelope on failure when `--json` is active.

### Testing (G19)
- Zero integration or behavioral tests exist. An extensive matrix of crash-injection, concurrency, multirepo, idempotency, and purity tests is needed.
