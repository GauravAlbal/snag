# Support

## Where to get help

- **Bugs and questions**: open a GitHub issue using the issue templates.
- **Security vulnerabilities**: report privately — see [SECURITY.md](SECURITY.md).
- **Discussion**: the project's GitHub Discussions.

## What is supported

- The **latest tagged release** on macOS and Linux.
- The documented CLI surface (`snag report/list/show/context/export/backup/
  restore/rebuild/verify/doctor/retract`) and the versioned context/export
  protocols ([docs/STABILITY.md](docs/STABILITY.md)).

## What is not supported

- Older releases (fixes land on the latest release only).
- Native Windows (not a target; WSL on Windows is untested).
- The internal SQLite schema as an API — downstream systems must consume
  `snag export`.

## When you file an issue, include

- `snag --version` and OS/architecture;
- install method (installer / cargo / release binary / built from source);
- `snag doctor` output with sensitive fields redacted;
- whether `snag verify --full` passes;
- reproduction steps.

**Never upload your database or full store.** Export the relevant records
(`snag export --after-sequence N`) and redact them.

## Response expectations

This is a small project. Issues are triaged in batches rather than in real
time; a clear, complete report (per the templates) gets answered faster than
one requiring back-and-forth.
