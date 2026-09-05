# Changelog

## 0.2.0

- Bundle the complete pinned Codex app-server 0.153.3 runtime on all four release
  targets, including code-mode host, ripgrep, and platform sandbox/shell helpers.
- Distribute one native self-extracting executable per platform, not installation
  archives. Extract the embedded runtime into a private, content-hash-keyed cache
  without network downloads; verify cached files before reuse and fail closed on
  corruption rather than falling back to a different Codex on PATH.
- Add `--extract-runtime` for inspecting packaged notices/source metadata and
  `CODEX_ACP_CACHE_DIR` for selecting a private extraction cache.
- Preserve explicit full-CLI overrides through `--codex-path` / `CODEX_PATH`,
  and add standalone overrides through `--app-server-path` /
  `CODEX_APP_SERVER_PATH`. Plain Cargo builds still require an explicit backend
  or an assembled package.
- Include upstream notices, source material, and rebuild/relink inputs; verify
  bundled startup and execution with isolated, credential-free model fixtures.

Model authentication/configuration and platform runtime requirements still
apply. The bundled app-server is not the interactive Codex CLI. Linux packages
retain upstream zsh's glibc 2.38 and libtinfo 6 requirements despite the adapter's
static musl target.

## 0.1.1

First published binary release of the Rust ACP protocol v2 adapter for Codex
app-server. Fixes draft-release lookup in the publisher while retaining the
original `v0.1.0` source tag unchanged.

- ACP v2 sessions, prompts, steering, cancellation, configuration, history replay,
  fork, close, and archival-based deletion.
- Stable message/tool/terminal projections, plans, usage, diagnostics, and
  backend-verified descendant activity.
- Precise approvals and supported elicitations; separate session defaults,
  prompt controls, and targeted live-turn settings.
- Negotiated bidirectional Codex extensions with explicit ownership, callback,
  history-reset, and host-authority controls.
- Session-owned native MCP-over-ACP bridging alongside stdio and HTTP MCP.
- Bounded independent event delivery and Codex-owned context management.
- Credential-free protocol tests and opt-in installed-Codex workflows using local
  model fixtures.
- Automated checks and binary releases for Apple Silicon macOS, Linux x86-64
  and ARM64, and Windows x86-64, including checksums, provenance, and notices.

ACP v2 and its optional SDK features are draft and pinned. Backend features still
depend on the installed Codex version, configuration, model, and account.

## 0.1.0 — tagged, not published

The initial source tag is retained for traceability. All four release binaries
were built and attested, but a draft-release lookup failure prevented binary
publication. Use the `v0.1.1` release instead.
