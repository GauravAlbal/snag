# The Snag context protocol

The `SNAG_CONTEXT_FILE` environment variable points at an optional JSON
document that attaches session context — session, task, tool, model, repository
identity — to every `snag report` run that inherits it. It is how a coding
agent (or a wrapper around one) tells Snag *what was happening* when an
observation was captured. No private tooling is required: the file is plain
JSON, versioned, and documented below.

## The schemas

| File | Validates |
|---|---|
| [schemas/snag-context-v1.schema.json](../schemas/snag-context-v1.schema.json) | The `SNAG_CONTEXT_FILE` document |
| [schemas/observation-input-v1.schema.json](../schemas/observation-input-v1.schema.json) | The JSON document `snag report --json` accepts (legacy; ownership must come from CLI) |
| [schemas/observation-input-v2.schema.json](../schemas/observation-input-v2.schema.json) | The JSON document `snag report --json` accepts (current; requires explicit `owner` or `unowned`) |
| [schemas/export-stream-v1.schema.json](../schemas/export-stream-v1.schema.json) | Every line of a `snag export` stream |

All four are JSON Schema draft-07. The context schema is enforced by the
binary (schema_version), the observation and export schemas are the documented
contracts with a compatibility test suite (`tests/schema_compat.rs`) that
validates the binary's real output against them. The v2 observation schema
adds exactly one of `"owner": "<repository>"` or `"unowned": true` as
required; v1 remains accepted only when the CLI supplies `--owner` or
`--unowned` (see [Ownership on every capture](#ownership-on-every-capture)).

The export header keeps the `export_schema_version` vocabulary at `1` and
advertises the required reader through `minimum_reader_version`: `1` for
legacy observation streams, `2` when remediation records are present, and `3`
when an `observation_created` payload carries `owner_repository_id` or
`owner_was_explicitly_unowned`, or an `observation_owner_assigned` record is
present. Consumers must honor this field and preserve ownership fields when
transforming records.

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
- An explicit repository ID still identifies the filing repository. If the
  current Git checkout is already bound to a different repository ID, Snag
  keeps the explicit attribution but does not attach that checkout, worktree,
  or its remote aliases; it emits a warning instead. Use `--owner` to name a
  different repository that owns the fix.

## Ownership on every capture

Every `snag report` MUST declare exactly one fix owner. Reporter location
(cwd, current checkout) is not ownership: it records where the observation
was filed FROM, not which lane should remediate it. Guessing `current` to
mean "my checkout" recreates the misrouting the explicit flag exists to
prevent.

There are three equivalent ways to declare it. Pick the one your pipeline
supports; the persisted `owner_repository_id` is the same in all three:

1. **CLI flag (preferred for humans and shells):**

   ```sh
   snag report "<title>" --owner owner/repository ...    # known
   snag report "<title>" --unowned ...                    # ambiguous / environmental
   ```

   `--owner <id|alias|path|current>` and `--unowned` are mutually exclusive;
   the CLI value is the one complete choice and overrides any JSON or prose
   declaration. Empty `--owner ""` and JSON/prose `unowned: false` do NOT
   satisfy the requirement.

2. **Prose stdin (`snag report --stdin`):** an `Owner:` section containing one
   repository value, OR an `Unowned:` section containing literal `true`.

3. **JSON intake (`snag report --json <file>`):** schema_version `2` with
   exactly one of `"owner": "<repository>"` or `"unowned": true`. The
   observation-input-v2 schema is published at
   `schemas/observation-input-v2.schema.json`. JSON v1 input is still
   accepted at the runtime, but only when the CLI supplies `--owner` or
   `--unowned` — a v1 document that omits ownership is rejected.

Errors clearly name the escape hatch:

```text
Validation error: ownership is required: pass --owner <repository> or --unowned
```

Persisted explicit-unowned observations keep `owner_repository_id = None`,
set `owner_was_explicitly_unowned = true` as an audit marker, and never gain a
phantom owner. They can be moved into a lane later via the append-only
`snag review assign-owner` event without rewriting the original record.

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
