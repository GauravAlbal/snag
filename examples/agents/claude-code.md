# Snag for Claude Code

Snag is a durable observation outbox: it records out-of-scope bugs and friction
with evidence so findings survive the session.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/13banditos/snag/master/install.sh | bash
```

## Instruction block

Add this to your CLAUDE.md (or the project's agent instructions):

```text
When you encounter a material bug, regression, misleading interface, recurring
workaround, or tooling failure outside the current task, record it with `snag`
while the evidence is fresh, then continue the assigned task.

Do not broaden the current task to fix the snag unless explicitly asked.
Do not report ordinary implementation errors or your own transient mistakes.
```

## Optional context setup

Write a per-session context file and point `SNAG_CONTEXT_FILE` at it (for
example from a shell wrapper around `claude`):

```json
{
  "schema_version": 1,
  "source": {
    "kind": "agent_explicit",
    "agent_runtime": "claude-code",
    "model": "<model id>"
  },
  "execution": {
    "session_id": "<session id>",
    "tool_name": "bash"
  }
}
```

If you do not set a context file, Snag still attaches git repository identity
automatically, and `SNAG_SOURCE_KIND=agent_report` + `SNAG_REPORTER_ID=<name>`
mark captures as agent-produced.

## Completion-review pattern

At the end of a task, review what was captured and triage it:

```bash
snag list --since 1d          # everything captured today
snag show <observation-id>    # inspect one finding
snag export --output findings.jsonl   # hand the stream to your issue tracker
```

## Privacy

Nothing is uploaded anywhere. Observations stay in the local store
(`snag doctor` prints the path); export is an explicit, local action.
