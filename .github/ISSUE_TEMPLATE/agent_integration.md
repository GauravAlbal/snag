---
name: Agent integration
description: Snag did not behave as expected inside a coding-agent workflow
title: "[agent-integration] "
labels: [agent-integration]
body:
  - type: input
    id: version
    attributes:
      label: Snag version
      description: Output of `snag --version`
    validations:
      required: true
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
  - type: dropdown
    id: agent
    attributes:
      label: Agent / runtime
      options: [Claude Code, Codex CLI, Gemini CLI, OpenCode, Generic AGENTS.md, Other]
    validations:
      required: true
  - type: textarea
    id: setup
    attributes:
      label: Where the instruction was added
      description: Which file (AGENTS.md, CLAUDE.md, settings, wrapper script) and what it contains (redacted).
    validations:
      required: true
  - type: textarea
    id: behavior
    attributes:
      label: What happened
      description: Did the agent run `snag`? What output? Did the observation persist?
    validations:
      required: true
  - type: textarea
    id: context
    attributes:
      label: Context-file setup
      description: If you set SNAG_CONTEXT_FILE, paste the document (redacted) and the output of `snag context` (redacted).
