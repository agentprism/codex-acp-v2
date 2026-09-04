# Codex ACP v2 development

## Purpose and architecture

Build a working ACP protocol v2 agent in Rust on top of the Codex app-server.
Codex owns inference, model history, tools, permissions, execution environments,
and context management. This project owns protocol adaptation, client interaction,
session projections, and negotiated extensions. Do not reimplement the agent loop
or feed replayed UI history back into model context.

Use the real `agent-client-protocol` v2 surface (`schema::v2`, `Agent.v2()` and
`unstable_protocol_v2`). The SDK package version alone does not select wire v2.
Keep the draft SDK pinned and commit Cargo.lock. The reference Codex source is
https://github.com/openai/codex at the revision documented in README.md. A local
checkout is an optional read-only reference, never a build-time path dependency.

## Idiomatic modern Rust

- Use the pinned latest stable toolchain and Rust 2024 edition; no nightly-only
  features or unnecessary compatibility scaffolding.
- Prefer clear ownership, borrowing, enums, newtypes, exhaustive matches, and
  small cohesive modules. Keep public APIs minimal and document their contracts.
- Use typed protocol structures at the ACP boundary. Preserve raw JSON only
  where an extensible downstream protocol genuinely requires it.
- Prefer standard-library facilities and existing dependencies over new ones.
  Add dependencies only when they materially simplify a necessary capability.
- Use structured errors with context. Never panic on peer input, silently swallow
  operational failures, or use `unwrap`/`expect` in fallible production paths.
- Prefer native async functions and RPITIT with explicit Send bounds for traits
  when needed; avoid async-trait and gratuitous boxing or dynamic dispatch.
- Do not hold synchronous mutex guards across await points. Keep connection
  dispatch responsive while inference, approvals, replay, and cancellation run.
- Bound queues, frames, retained content, and shutdown waits. Do not silently drop
  protocol events when limits are exceeded; fail explicitly or reconcile safely.
- Use inline format arguments, method references, and clear iterator operations
  where they improve readability. Avoid boolean positional API knobs when an enum
  or named method conveys the intent better.
- Keep unsafe code forbidden. Follow rustfmt and resolve Clippy warnings instead
  of broadly suppressing them.

## Protocol and security invariants

- ACP stdout is exclusively protocol traffic; diagnostics go to stderr.
- Prompt acceptance is not turn completion. Maintain stable session/message/tool
  identities and ordered projection across live events and history replay.
- Separate session defaults, per-prompt overrides, and live-turn settings.
- Preserve permission scope and cancellation. Never auto-approve as a fallback.
- Advertise only implemented capabilities. Reject unsupported parameters or
  operations explicitly instead of acknowledging work that was not performed.
- Codex-specific methods must be negotiated, underscore-prefixed extensions;
  custom data on standard ACP payloads belongs in namespaced `_meta`.
- Validate session ownership and distinguish session operations from host/account
  authority. Host process, filesystem, auth, and global configuration access must
  not implicitly inherit a conversation sandbox's authorization.
- Never log credentials or copy credentials into model-visible content.
- Persisted transcript replay is for the client only. Context-window rollover
  must not create a replacement ACP session or an adapter-authored summary.

## Test authoring guidelines

Be conservative by default: add only tests needed to protect meaningful behavior,
an important invariant, or a demonstrated regression. Coverage percentages and
test counts are not goals.

- Do not author tautological tests, tests of constants or declarations, assertions
  that restate fixture construction, or tests that merely exercise derives,
  getters, dependency behavior, or a mock's predetermined response in isolation.
- Do not duplicate cases already covered by a stronger test. Do not preserve tests
  for removed behavior or expand a test matrix without a concrete failure risk.
- Prefer a small number of integration tests through public protocol boundaries.
  Use deterministic fake app-server peers for ordering, callbacks, cancellation,
  reconnect failures, configuration scope, and replay behavior.
- Assert observable outputs and side effects independently of implementation
  details. Prefer comparing complete meaningful structures over scattered fields.
- Add unit tests only when a focused transformation or state transition is easier
  to verify that way and has a real correctness risk.
- Avoid timing sleeps, external network dependencies, paid inference, and machine
  credentials in default tests. Bound waits and coordinate tasks explicitly.
- Live Codex checks must be explicit opt-ins. Describe what they verified and
  what was not exercised; never report mocked tests as live-model verification.

## Workflow and collaboration

- Read this file before working. Use apply_patch for local source/document edits.
- Keep changes focused, preserve unrelated user edits, and never reset others'
  work. Do not add stubs, fake success paths, or unfinished advertised features.
- Run `cargo fmt --all`, `cargo check --all-targets`, `cargo test --all-targets`,
  and `cargo clippy --all-targets -- -D warnings` as appropriate before handoff.
- Commit coherent completed work with descriptive messages. Stage explicit paths;
  never use broad staging that might include another agent's unfinished changes.
- Keep Linux, Apple Silicon macOS, and Windows working. Use portable paths and
  platform-aware fixtures; explicitly mark tests that require Unix execution.
- Update release notes for user-visible changes. Keep CI actions pinned by commit,
  tool versions explicit, and release packaging aligned with the documented targets.
- The primary agent establishes the foundation, then acts as orchestrator. Use
  implementation subagents with explicit file ownership and agreed interfaces.
  Integration and verification are also delegated. Coordinate shared-file edits
  and commits; all agents share the working tree.
