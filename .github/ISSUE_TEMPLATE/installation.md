---
name: Installation problem
description: Snag will not install or run on your machine
title: "[installation] "
labels: [installation]
body:
  - type: input
    id: version
    attributes:
      label: Snag version (if installed)
      description: Output of `snag --version`, or "not installed"
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
      label: Install method attempted
      options: [installer (curl), cargo install --git, cargo install --path, release binary, other]
    validations:
      required: true
  - type: textarea
    id: error
    attributes:
      label: Error output
      description: Paste the full error output from the failed install.
    validations:
      required: true
  - type: input
    id: dest
    attributes:
      label: Install destination
      description: Where did you try to install? (`~/.local/bin`, `--dest`, `--system`?)
  - type: textarea
    id: env
    attributes:
      label: Environment notes
      description: Anything unusual — proxy, locked-down shell, non-standard PATH, container, Nix?
