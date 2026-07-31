# Conduit Development Guidelines

This document defines the engineering standards for all contributors and AI coding agents working on Conduit.

These guidelines are mandatory unless explicitly overridden.

---

# Core Philosophy

Conduit values:

- Correctness over cleverness
- Simplicity over abstraction
- Composition over inheritance
- Streaming over buffering
- Explicitness over magic
- Small changes over massive rewrites
- Documentation over tribal knowledge

Every change should leave the project in a better state than it was found.

---

# Development Workflow

## Test-Driven Development

All new functionality should follow TDD whenever practical.

The preferred workflow is:

1. Write a failing test.
2. Verify the test fails.
3. Implement the smallest amount of code necessary.
4. Verify the test passes.
5. Refactor.
6. Repeat.

Avoid writing large amounts of production code before tests exist.

Tests should describe behavior rather than implementation.

---

## Atomic Commits

Commits should represent a single logical change.

Good examples:

- add streaming whisper provider
- implement speaker enrollment API
- fix websocket reconnect race
- add conversation timeout tests

Bad examples:

- misc fixes
- update everything
- WIP
- more work
- final changes

Every commit should:

- build successfully
- pass all tests
- leave the repository in a releasable state

Avoid long-lived work that cannot be safely merged.

---

## Commit Early

Do not wait until an entire feature is complete before committing.

Commit whenever a logical piece of work has been completed.

Examples:

- parser implemented
- API implemented
- tests passing
- documentation written
- UI completed

Small commits are easier to review, debug, and revert.

---

# Testing

Every feature should include tests.

Types of tests include:

- unit tests
- integration tests
- API tests
- serialization tests
- concurrency tests
- property tests where appropriate
- end-to-end tests for major workflows

Bug fixes should include regression tests.

Never fix a bug without preventing it from returning.

---

# Documentation

Documentation is considered part of the implementation.

Whenever behavior changes, update:

- README
- architecture documentation
- API documentation
- examples
- configuration reference
- migration guides when applicable

Outdated documentation is a bug.

---

# Code Quality

Favor:

- readable code
- descriptive names
- explicit control flow
- small functions
- immutable data where practical

Avoid:

- unnecessary abstraction
- premature optimization
- deeply nested conditionals
- hidden side effects
- global mutable state

If code requires extensive comments to understand, simplify the code.

---

# Error Handling

Never silently ignore errors.

Errors should:

- include useful context
- preserve root causes
- be actionable
- be logged appropriately

Avoid panic except for unrecoverable programmer errors.

---

# Logging

Use structured logging.

Every log entry should include useful context.

Avoid logs like:

```
Error occurred
```

Prefer:

```
Failed to connect to Whisper provider.

provider=local
host=whisper:9000
attempt=3
error=connection refused
```

Logs should help diagnose production issues without reproducing them locally.

---

# Observability

Every significant operation should expose:

- metrics
- traces
- logs

Long-running operations should expose progress.

Every request should be traceable across services.

---

# Performance

Measure before optimizing.

Do not introduce complexity based on assumptions.

When optimizing:

1. benchmark
2. identify bottleneck
3. optimize
4. benchmark again

Prefer algorithms over micro-optimizations.

---

# APIs

Public APIs should:

- be documented
- be versioned
- remain backward compatible whenever practical
- return consistent errors

Breaking changes require justification.

---

# Streaming First

Whenever possible:

Do not buffer complete responses.

Instead:

- stream tokens
- stream transcripts
- stream events
- stream audio
- stream progress

Users should receive information as soon as it exists.

---

# Event-Driven Design

Every important state transition should emit an event.

Examples:

- conversation started
- speech partial
- speech final
- tool executing
- tool completed
- TTS started
- TTS completed

Avoid tightly coupling components through synchronous APIs when an event is more appropriate.

---

# Configuration

Configuration should be:

- explicit
- documented
- validated
- reloadable where practical

Avoid hidden configuration.

---

# Security

Never:

- commit secrets
- hardcode credentials
- log tokens
- log passwords
- log API keys

Validate all external input.

Prefer least privilege.

---

# Dependencies

Before introducing a dependency ask:

- Does the standard library already solve this?
- Is the dependency actively maintained?
- Is it widely adopted?
- Is it necessary?
- Can we easily replace it later?

Every dependency increases maintenance cost.

---

# Rust Guidelines

Prefer:

- ownership over shared mutability
- Result over panic
- enums over boolean flags
- builders for complex configuration
- traits for provider interfaces

Use:

- clippy
- rustfmt
- cargo test
- cargo audit

Warnings should generally be treated as errors.

---

# Reviews

Before considering work complete verify:

- builds successfully
- tests pass
- formatting applied
- linting passes
- documentation updated
- examples updated if needed
- no TODOs left without associated issues

---

# Pull Requests

Each pull request should answer:

- What changed?
- Why?
- How was it tested?
- Does it introduce breaking changes?
- Does documentation need updating?

Keep pull requests focused.

Smaller pull requests are strongly preferred over large ones.

---

# AI Agent Expectations

AI coding agents should:

- understand existing architecture before changing it
- preserve project conventions
- avoid speculative refactoring
- avoid unrelated cleanup
- avoid changing formatting outside touched files
- avoid introducing unnecessary dependencies

Agents should prefer extending existing abstractions over creating parallel implementations.

When uncertain, choose the simplest solution that satisfies current requirements.

---

# Agent Skills

Per-repo configuration for agent workflow skills. Each subsection states the
policy; the referenced file holds the command-level reference.

## Issue tracker

Issues and PRDs live as GitHub issues in `constructorfleet/conduit`, managed
with the `gh` CLI. Pull requests are not treated as a request surface.

This tracker is **public** — see [`docs/agents/issue-tracker.md`](docs/agents/issue-tracker.md).

## Triage labels

Triage uses five roles: `needs-triage`, `needs-info`, `ready-for-agent`,
`ready-for-human`, `wontfix`. See [`docs/agents/triage-labels.md`](docs/agents/triage-labels.md).

## Domain docs

Single-context: one `CONTEXT.md` and one `docs/adr/` at the repository root
cover the whole workspace. Both are created lazily — their absence is not a
defect. See [`docs/agents/domain.md`](docs/agents/domain.md).

---

# Definition of Done

A task is complete only when:

- functionality is implemented
- tests exist and pass
- documentation is updated
- examples are updated if necessary
- linting passes
- formatting passes
- the project builds successfully
- changes are committed as a logical atomic commit
- no known regressions have been introduced

Implementation alone does not constitute completion.