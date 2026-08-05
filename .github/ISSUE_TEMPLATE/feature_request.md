---
name: Feature proposal
description: A change to Snag's behavior or surface
title: "[feature] "
labels: [feature]
body:
  - type: input
    id: version
    attributes:
      label: Snag version
      description: Output of `snag --version` (if installed)
  - type: input
    id: os
    attributes:
      label: OS / architecture
    validations:
      required: true
  - type: dropdown
    id: install
    attributes:
      label: Install method
      options: [installer, cargo install, release binary, built from source]
    validations:
      required: true
  - type: textarea
    id: problem
    attributes:
      label: Problem
      description: What are you trying to do that Snag cannot do today?
    validations:
      required: true
  - type: textarea
    id: proposal
    attributes:
      label: Proposed behavior
      description: Concrete, minimal description of the change.
    validations:
      required: true
  - type: textarea
    id: alternatives
    attributes:
      label: Alternatives considered
      description: Other ways you could solve this without changing Snag.
  - type: textarea
    id: scope
    attributes:
      label: Scope check
      description: Does this fit Snag's non-goals (README)? Snag is not an issue tracker, orchestrator, analytics, tracing, or telemetry platform, and not an LLM-based deduplicator. If this proposal makes Snag one of those, explain why it still belongs here.
    validations:
      required: true
