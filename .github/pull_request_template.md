---
name: Pull request
about: Changes to nexus-chat
title: "prefix: short imperative summary"
labels: []
---

## What

One short paragraph: what this changes and why. Follow the commit-style
conventions — lowercase conventional prefix (`feat:` / `fix:` / `refactor:` /
`design:` / `docs:` / `style:` / `test:` / `chore:` / `build:`), imperative
tone, em-dash detail.

## Changes

- bullet list of the notable pieces (module moves, new commands, behavior
  changes, API surface)

## Test plan

- [ ] `scripts/check.sh` runs green locally — fmt + clippy (`-D warnings`,
      pedantic) + `cargo audit` + the full test suite. This is the gate the
      pre-commit hook enforces on master.
- [ ] Tests are hermetic (no network, no real key material, temp dirs for
      anything touching disk).

## Notes for reviewers

- Anything reviewers should look at closely, e.g.:
  - behavior-preservation concerns (viewport pinning, streaming, popup flows)
  - deliberate deviations from the plan/spec and why
  - follow-up work intentionally left out of this PR

## Release notes

One line in conventional-commit form that can be dropped into the release
notes (generated from commits since the last tag). Leave blank if this PR
doesn't warrant a note.
