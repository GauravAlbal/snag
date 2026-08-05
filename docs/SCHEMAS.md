# The Snag context protocol

The `SNAG_CONTEXT_FILE` environment variable points at an optional JSON
document that attaches session context — session, task, tool, model, repository
identity — to every `snag report` run that inherits it. It is how a coding
agent (or a wrapper around one) tells Snag *what was happening* when an
observation was captured. No private tooling is required: the file is plain
JSON, versioned, and documented below.

## The schema

| File | Validates |
|---|---|
| [schemas/snag-context-v1.schema.json](../schemas/snag-context-v1.schema.json) | The `SNAG_CONTEXT_FILE` document |
| [schemas/observation-input-v1.schema.json](../schemas/observation-input-v1.schema.json) | The JSON document `snag report --json` accepts |
| [schemas/export-stream-v1.schema.json](../schemas/export-stream-v1.schema.json) | Every line of a `snag export` stream |

All three are JSON Schema draft-07. The context schema is enforced by the
binary (schema_version), the observation and export schemas are the documented
contracts with a compatibility test suite (`tests/schema_compat.rs`) that
validates the binary's real output against them.

## Required versus optional fields

The context document has **exactly one required field**: `schema_version`
(must be `1`). Everything else — `source`, `execution`, `repository`, `extra`,
`idempotency_key` — is optional. A document with an unsupported
`schema_version` is rejected with a typed error; it is never misparsed.

```json
{
  "schema_version": 1
}
```

That is a valid (minimal) context file: no context at all beyond "this is
protocol version 1".

## Precedence rules

Context is merged, later sources overriding earlier ones:

```text
explicit CLI flags  >  context file  >  environment variables  >  git auto-detect
```

- `--repo-id`, `--session-id` (and the other explicit flags) beat the context
  file.
- The context file replaces the environment-derived `source` and overlays
  `execution`/`repository` fields present in it.
- Git identity (repository/checkout/worktree IDs, branch, HEAD) is
  auto-detected from the current checkout and used as the baseline.

Run `snag context` to see the effective result of this merge from the current
process.

## Compatibility and versioning rules

1. `schema_version` must be `1`. A future major version is rejected with a
   typed error rather than misparsed.
2. **Unknown fields are ignored** at every level (documented compatibility
   rule). The example below includes a `task_id` key the schema does not
   define — a wrapper may attach its own identifiers this way. If you want
   wrapper data persisted on the observation, put it under `extra` instead,
   which is stored verbatim.
3. The document is read fresh on every `snag report` invocation — there is no
   daemon and no caching.
4. The observation JSON, context JSON, and export stream are versioned public
   contracts ([STABILITY.md](STABILITY.md)); the SQLite schema is not.

## Complete example

```json
{
  "schema_version": 1,
  "source": {
    "kind": "agent_explicit",
    "agent_runtime": "claude-code",
    "model": "..."
  },
  "execution": {
    "session_id": "...",
    "task_id": "...",
    "tool_name": "bash"
  }
}
```

`task_id` is a wrapper-owned key: unknown keys are ignored by the reader, so
it is safe to include; if you need it persisted on observations, mirror it
under `extra`.

## Legacy-compatible extension fields

`execution.workspace_id`, `execution.program_id`, `execution.task_id`,
`execution.attempt_id`, and `execution.authority_sequence` are optional
extension fields used by wrapper orchestrators. They are part of the schema
for backward compatibility but are **not** the primary public contract —
generic wrappers should prefer `session_id`, `tool_name`, `tool_invocation_id`,
`command_shape`, and `extra`. `task_id` is a generic work-item identifier: a
wrapper that tracks the task an agent was executing may attach it here.

## Validating a context file

```sh
# with python + jsonschema installed
python3 -c "import json,jsonschema,sys; jsonschema.validate(json.load(open('ctx.json')), json.load(open('schemas/snag-context-v1.schema.json')))"
```

The in-repo compatibility test (`tests/schema_compat.rs`) validates the
binary's actual `snag context` output and real export streams against these
schemas on every `cargo test` run.

## Setting it up for an agent

See [examples/agents/](../examples/agents/) for copy-paste integration blocks
per agent (Claude Code, Codex, Gemini CLI, OpenCode, generic shell agents).
The short version: point `SNAG_CONTEXT_FILE` at a JSON file the wrapper writes
per session, and `snag report` will attach that context automatically.
