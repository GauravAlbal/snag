# Snag for any shell-based agent (generic)

This is the base integration for agents that run shell commands — or for an
`AGENTS.md`/`CLAUDE.md` that should work regardless of which agent reads it.
No wrapper, no private tooling, no context file required.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/GauravAlbal/snag/main/install.sh | bash
```

## Instruction block (drop into AGENTS.md / CLAUDE.md)

```text
When you find a bug, broken command, misleading success message, recurring
workaround, or tool failure outside the current task:

1. Decide who owns the fix BEFORE recording:
   - Known: pass `--owner <owner/repo>` (id, alias, path, or `current`).
   - Genuinely ambiguous / environmental: pass `--unowned`.
   Your reporting location is NOT ownership; guessing `current` recreates the
   misrouting the explicit owner flag exists to prevent.
2. Record it with `snag` while the evidence is fresh.
3. Continue the current task.

Do not fix the unrelated problem unless the user explicitly asks you to.
Do not report ordinary implementation mistakes that belong to the current task.
Empty owner and `unowned: false` do not satisfy the requirement; one of the
two flags above is always required. JSON intake (`--json`) uses schema v2 with
exactly one of `"owner": "..."` or `"unowned": true`.
```
## Optional context setup (environment variables)

No context file needed. Mark captures as agent-produced with environment
variables:

```bash
export SNAG_SOURCE_KIND=agent_report
export SNAG_REPORTER_ID="my-agent"
```

Git repository identity (repo/checkout/worktree IDs, branch, HEAD) is attached
automatically from the current checkout. For richer session context, write a
versioned JSON file and point `SNAG_CONTEXT_FILE` at it — see
[docs/SCHEMAS.md](../../docs/SCHEMAS.md).

## Reporting (every capture declares an owner)

Every `snag report` must declare exactly one fix owner. Pick `--owner` when
the lane is known, `--unowned` when the finding is genuinely ambiguous or
purely environmental. Reporter location (your current checkout) is not
ownership; guessing `current` recreates the misrouting the explicit flag
exists to prevent. Empty owner and `unowned: false` are rejected.

```bash
# Known owner — this is the lane that should fix the issue
snag report "build reports success but creates no artifact" \
  --owner GauravAlbal/snag \
  --kind bug --observed "..." --expected "..." --repro "..."

# Genuinely ambiguous / environmental — fix lane cannot be named
snag report "container hostname resolution flaky in CI" \
  --unowned \
  --kind friction --observed "..." --expected "..."
```

## Completion-review pattern

```bash
snag list --since 1d          # everything captured today
snag show <observation-id>    # inspect one finding
snag export --output findings.jsonl   # hand the stream to your issue tracker
```

## Privacy

Nothing is uploaded anywhere. Observations stay in the local store
(`snag doctor` prints the path); export is an explicit, local action.
