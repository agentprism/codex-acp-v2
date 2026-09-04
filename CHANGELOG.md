# Changelog

## 0.1.0

Initial release of the Rust ACP protocol v2 adapter for Codex app-server.

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
