---
name: Bug report
description: Something in Snag behaves incorrectly
title: "[bug] "
labels: [bug]
body:
  - type: markdown
    attributes:
      value: |
        Thanks for filing a bug. Do **not** upload your database or full store — export the relevant records (`snag export --after-sequence N`) and redact them instead.
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
      description: e.g. macOS 14 arm64, Linux x86_64
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
    id: doctor
    attributes:
      label: snag doctor output
      description: Run `snag doctor` and paste the output with sensitive fields (paths, reporter ids) redacted.
    validations:
      required: true
  - type: dropdown
    id: verify
    attributes:
      label: Does `snag verify --full` pass?
      options: [Yes, No, Store does not exist]
    validations:
      required: true
  - type: textarea
    id: expected
    attributes:
      label: What did you expect?
    validations:
      required: true
  - type: textarea
    id: observed
    attributes:
      label: What happened instead?
    validations:
      required: true
  - type: textarea
    id: repro
    attributes:
      label: Reproduction steps
      description: Minimal commands that trigger the bug.
    validations:
      required: true
  - type: textarea
    id: impact
    attributes:
      label: Impact
      description: What did this cost you? Data loss, lost time, wrong output?
