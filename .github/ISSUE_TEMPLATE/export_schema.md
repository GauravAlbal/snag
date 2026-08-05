---
name: Export / schema compatibility
description: A consumer of the export stream or a published schema does not work
title: "[export-schema] "
labels: [export-schema]
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
  - type: input
    id: consumer
    attributes:
      label: Consumer language / runtime
      description: e.g. Python 3.12, Node 20, jq, your own validator
    validations:
      required: true
  - type: textarea
    id: sample
    attributes:
      label: Export stream sample (redacted)
      description: The header line and one record line, with sensitive payload values redacted.
    validations:
      required: true
  - type: textarea
    id: expected
    attributes:
      label: Expected vs actual
      description: Which field/version/type did you expect, and what did you get?
    validations:
      required: true
  - type: input
    id: schema
    attributes:
      label: Schema file and version
      description: e.g. export-stream-v1.schema.json, header
