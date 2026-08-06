# Snag for Gemini CLI

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

1. Record it with `snag` while the evidence is fresh.
2. Continue the current task.

Do not fix the unrelated problem unless the user explicitly asks you to.
Do not report ordinary implementation mistakes that belong to the current task.
```

## Optional context setup

Write a per-session context file and point `SNAG_CONTEXT_FILE` at it (for
example from a wrapper around `gemini`):

```json
{
  "schema_version": 1,
  "source": {
    "kind": "agent_explicit",
    "agent_runtime": "gemini-cli",
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

## Completion-review pattern

```bash
snag list --since 1d          # everything captured today
snag show <observation-id>    # inspect one finding
snag export --output findings.jsonl   # hand the stream to your issue tracker
```

## Privacy

Nothing is uploaded anywhere. Observations stay in the local store
(`snag doctor` prints the path); export is an explicit, local action.
