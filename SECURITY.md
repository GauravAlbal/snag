# Security Policy

## Reporting a vulnerability

Please report security vulnerabilities **privately** — do not file a public
issue. Use GitHub's private vulnerability reporting (Security tab →
"Report a vulnerability" on the repository), or email the maintainers directly
if private reporting is unavailable. Include:

- the affected version (`snag --version`);
- a minimal reproduction (commands run, expected vs observed);
- whether the issue involves the store, attached artifacts, or the export
  stream.

We will acknowledge within 5 business days and coordinate a fix and release
before public disclosure. Public disclosure happens only after a fixed release
exists.

## Supported versions

Only the **latest tagged release** is supported. Older releases receive fixes
only when they are re-released as the latest. Report issues against the latest
version if at all possible.

## Local-store threat model

Snag stores observations **locally on the machine where they were captured**.
There is no server component, no account, and no cloud.

- **Data at rest is not encrypted.** Anyone with read access to the store
  directory can read observations and any attached artifacts. Protect the
  store directory the way you protect a code review log.
- **The store is not a secrets vault.** Do not attach files containing
  credentials or keys unless you accept that they will be stored in plaintext
  on disk.
- **Git remotes, paths, branch names, model/tool IDs, and context may be
  stored** in observation records. The store is a faithful log of what the
  agent was working on.

## Artifact sensitivity

Artifacts are **copied** into the content-addressed object store
(`objects/blake3/...`) **only when explicitly attached** with `--artifact`.
The originals are never moved or modified. Because copies are verbatim, an
attached file may contain secrets; attach only what is needed, and treat the
store accordingly.

## Database and backup handling

- Backups (`snag backup`) are self-contained point-in-time bundles containing
  **everything** in the store: the database, the manifest, and all artifact
  objects. Protect backups like the store itself.
- `snag restore` is non-destructive by construction: it verifies the backup
  fully, restores into a temporary candidate, runs full verification on the
  candidate, and only then atomically switches. The pre-restore database is
  preserved as a forensic copy under `forensics/`.
- **Never delete the only copy of a store before a verified backup exists.**

## Hard-redaction limitations

The store is **append-only by design**:

- `snag retract <id>` appends a retraction record and **never deletes or
  rewrites** the original observation.
- There is no in-place editing, no deletion, and no purge command. To correct
  a record, retract it and file a new one.
- This means a record that should never have existed (e.g. one containing an
  accidentally attached secret) **cannot be removed** by Snag itself. If that
  happens, delete the store directory (after taking any observations you want
  to keep via `snag export`) — the export stream is the portable record.

## Privacy guarantees

- Snag is local-only by default.
- It sends **no telemetry** and never uploads observations automatically.
- It does **not** capture environment variables or shell history.
- Artifacts are copied only when explicitly attached.
- Git remotes, paths, branch names, model/tool IDs, and context **may** be
  stored — see the local-store threat model above.
