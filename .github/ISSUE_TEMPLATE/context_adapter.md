---
name: Context adapter request
description: SNAG_CONTEXT_FILE context is missing, wrong, or unclear for your wrapper
title: "[context] "
labels: [context]
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
    id: wrapper
    attributes:
      label: Wrapper / tool producing context
      description: e.g. a shell wrapper, CI runner, session manager
    validations:
      required: true
  - type: textarea
    id: context_file
    attributes:
      label: SNAG_CONTEXT_FILE contents (redacted)
      description: Paste the document with sensitive values replaced.
    validations:
      required: true
  - type: textarea
    id: effective
    attributes:
      label: `snag context` output (redacted)
      description: What Snag actually attached.
    validations:
      required: true
  - type: textarea
    id: gap
    attributes:
      label: What was missing or wrong
      description: Which fields were dropped, overridden, or surprising?
    validations:
      required: true
