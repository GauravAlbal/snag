# Roadmap

Narrow on purpose. Snag is a durable observation outbox, not an "agent
observability platform". The roadmap stays small until the core job is proven
in the wild.

## v0.1 — durable local capture (shipped)

- Hash-chained, append-only local store with crash safety and read purity.
- Automatic git context; versioned `SNAG_CONTEXT_FILE` context protocol.
- Deterministic JSONL export with partial-window support (`--after-sequence`).
- Full recovery chain: backup → restore (non-destructive) → rebuild from
  export; `verify --full` over the whole chain.
- 71-test certified surface, three hard gates, CI on macOS + Linux.

## v0.2 — generic context adapters and installation polish

- More agent integrations (see [examples/agents/](../examples/agents/)) and
  adapter guidance for custom wrappers.
- Installation polish: evaluate crates.io, Homebrew tap, `cargo-binstall`,
  and Nix packaging; signed release artifacts.
- Schema/export compatibility tooling for downstream consumers.

## v0.3 — optional downstream sink protocol

- A documented push interface so observations can flow to issue trackers,
  analysis systems, or dashboards without custom polling.
- The interface stays optional and generic — no single vendor sink is promised
  as an OSS roadmap item.

## Explicitly not planned

Snag will not become: an issue tracker, an agent orchestrator, an analytics
system, a tracing platform, a telemetry collector, an automatic bug detector,
or an LLM-based deduplicator. Those jobs belong to downstream tools that
consume the export stream.
