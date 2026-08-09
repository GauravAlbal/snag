# Snag for OpenCode

Snag records out-of-scope bugs and friction with evidence so findings survive
the session.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/GauravAlbal/snag/main/install.sh | bash
```

## Instruction block

Add this to your AGENTS.md (or the project's agent instructions):
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

## Optional context setup

Write a per-session context file and point `SNAG_CONTEXT_FILE` at it (for
example from a wrapper around `opencode`):

```json
{
  "schema_version": 1,
  "source": {
    "kind": "agent_explicit",
    "agent_runtime": "opencode",
    "model": "<model id>"
  },
  "execution": {
    "session_id": "<session id>",
    "tool_name": "bash"
  }
}
```

Without a context file, Snag still attaches git repository identity
automatically; set `SNAG_SOURCE_KIND=agent_report` and
`SNAG_REPORTER_ID=<name>` to mark captures as agent-produced.

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
snag retract <observation-id> # retract a false positive (append-only)
```

## Privacy

Nothing is uploaded anywhere. Observations stay in the local store
(`snag doctor` prints the path); export is an explicit, local action.
