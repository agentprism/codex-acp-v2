# Codex ACP v2

[![CI](https://github.com/agentprism/codex-acp-v2/actions/workflows/ci.yml/badge.svg)](https://github.com/agentprism/codex-acp-v2/actions/workflows/ci.yml)

A Rust ACP **protocol v2** agent that delegates execution to a Codex app-server
child. Codex owns inference, tools, sandboxing, MCP, subagents, model history, and
context management. This adapter owns ACP sessions, configuration translation,
client-visible history, interaction routing, and negotiated Codex extensions.

This uses `Agent.v2()` and `schema::v2`, with the SDK's `unstable_protocol_v2`
feature. It is not a protocol-v1 adapter merely using a version-2 Rust package.
The draft SDK is pinned to `agent-client-protocol = 2.1.0`; `Cargo.lock` pins the
schema to `1.7.0`. Rust `1.98.1` and edition 2024 are pinned in the repository.

This is an independent AgentPrism project, not an official OpenAI product. It
requires an ACP **v2** client; clients supporting only ACP v1 cannot connect.

## Install a release

Download the **native executable** for your machine from
[GitHub Releases](https://github.com/agentprism/codex-acp-v2/releases). Each is one
self-extracting binary containing the adapter, complete pinned **Codex app-server
0.153.3** runtime, helpers, notices, and build metadata. There is no install tarball
to unpack and no backend download at startup. You do not need a separate Codex,
Node.js, or Rust installation. Model access still requires your own configured
provider/account.

| Platform | Release target | Download |
| --- | --- | --- |
| macOS, Apple Silicon only | `aarch64-apple-darwin` | Native executable, no extension |
| Linux, x86-64 | `x86_64-unknown-linux-musl` | Native executable, no extension |
| Linux, ARM64 | `aarch64-unknown-linux-musl` | Native executable, no extension |
| Windows, x86-64 | `x86_64-pc-windows-msvc` | Native `.exe` |

Linux adapter binaries use static musl linking, and Windows adapter binaries
statically link the MSVC runtime. The bundled upstream helpers have their own
system-library requirements; a `musl` target name does not mean every bundled
helper runs on a musl-only distribution. In particular, the packaged patched zsh
requires glibc 2.38 or newer and `libtinfo.so.6` on both Linux architectures;
the ARM64 `rg` also uses glibc and `libgcc_s.so.1`. Use a glibc distribution with
these libraries to use the complete runtime. Host sandbox facilities, shells,
Git, and toolchains used by your projects are still operating-system prerequisites.
Intel macOS and native Windows ARM64 releases are not provided. Our macOS
adapter executable is not Developer ID
signed or notarized, and our Windows adapter is not Authenticode signed. Bundled
upstream executables are unchanged and retain any upstream signatures.
Operating-system security prompts may therefore apply; verify provenance before
execution, and do not disable system-wide security checks.

For example, with the [GitHub CLI](https://cli.github.com/), download and verify
the Apple Silicon executable:

```sh
gh release download v0.2.0 --repo agentprism/codex-acp-v2 \
  --pattern codex-acp-v2-v0.2.0-aarch64-apple-darwin \
  --pattern SHA256SUMS
gh attestation verify codex-acp-v2-v0.2.0-aarch64-apple-darwin \
  --repo agentprism/codex-acp-v2
shasum -a 256 codex-acp-v2-v0.2.0-aarch64-apple-darwin
```

Compare the printed digest with that executable's entry in `SHA256SUMS`. On Linux
use `sha256sum`; on Windows use PowerShell's `Get-FileHash -Algorithm SHA256`.
Checksums detect changed bytes; the separate
[GitHub attestation verification](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations)
checks the artifact's recorded GitHub build provenance. An attestation is not an
OS signature or proof that the software is free of vulnerabilities.

After both checks succeed, make the Unix download executable:

```sh
chmod +x codex-acp-v2-v0.2.0-aarch64-apple-darwin
./codex-acp-v2-v0.2.0-aarch64-apple-darwin --help
```

Give your ACP client the absolute path to the downloaded executable. You can
rename it to `codex-acp-v2` (`codex-acp-v2.exe` on Windows) and put it on PATH.
The single executable can be moved or copied without a sidecar directory.
Windows downloads run directly; there is no archive-extraction step.

On first use the adapter extracts its embedded runtime into a private per-user
cache. Installation uses a temporary staging directory and atomic rename; cache
directories are keyed by the embedded payload's SHA-256. Each launch validates
cached file sizes, hashes, and executable modes where supported and rejects
symlinks, Windows reparse points, or unsafe paths. Unix caches enforce private
permissions; Windows uses the user cache directory's inherited ACLs. Nothing is
downloaded. Linux retains bubblewrap and Codex's patched zsh;
Windows retains the command-runner and sandbox-setup helpers.

Default cache roots are `$XDG_CACHE_HOME/codex-acp-v2` (or
`$HOME/.cache/codex-acp-v2`) on Linux, `$HOME/Library/Caches/codex-acp-v2` on macOS,
and `%LOCALAPPDATA%\codex-acp-v2` on Windows. Set `CODEX_ACP_CACHE_DIR` to an
absolute path to select another private cache. This is separate from the Codex
account/profile directory controlled by `CODEX_HOME`.

To extract/verify the runtime and print its directory without starting ACP:

```sh
codex-acp-v2 --extract-runtime
```

That directory includes `licenses/`, build metadata, and the canonical
`codex/bin/`, `codex/codex-path/`, and `codex/codex-resources/` package. `--help`
and `--version` do not extract it. A corrupt cache fails explicitly rather than
switching to a Codex on PATH or silently deleting files. Stop affected adapter
processes, remove only the exact cache directory named in the error, and retry
to recreate it from the verified executable. Keep notices and corresponding
sources when redistributing. Release automation and maintainer steps are in
[CONTRIBUTING.md](CONTRIBUTING.md#releases).

## Build and run

Install Rust using rustup. A plain Cargo build produces the adapter only; use an
explicit compatible backend while developing, or assemble the complete release
executable using the release tooling and `bundled-backend` feature. Source builds
without that feature can also use the canonical sibling `codex/` layout.
Existing Codex configuration and authentication are reused through Codex's normal
profile (`CODEX_HOME` can select a separate
profile). The bundled app-server is not the interactive Codex CLI and does not
provide a `login` subcommand. A configured ACP client may use the negotiated
account/login extensions with operator-granted host authority; alternatively,
an existing Codex CLI can authenticate the same profile with `codex login`.
Configuring another provider follows Codex's own provider requirements.

```sh
cargo build --release --locked
./target/release/codex-acp-v2 --codex-path /absolute/path/to/codex
```

Configure your ACP v2 client to launch that executable on stdin/stdout. Stdout is
reserved for JSON-RPC traffic; diagnostics and child diagnostics use stderr.
The ACP interface is stdio-only, not a shared multi-tenant daemon. Each adapter
process owns one app-server process and can host multiple threads. Native
MCP-over-ACP declarations additionally create private, token-protected loopback
HTTP listeners for Codex's MCP transport; these do not expose the ACP server.

The default is the bundled standalone app-server. Explicit alternatives are
`--codex-path` / `CODEX_PATH` for a **full Codex CLI**, or
`--app-server-path` / `CODEX_APP_SERVER_PATH` for a **standalone app-server**.
The two overrides are mutually exclusive. On Windows, use native `.exe` paths,
not npm's `codex.cmd` shim. Overrides are intended for development or deliberate
backend management; their versions and dependencies are your responsibility.

Repeat `--codex-arg` for backend options. They precede `--listen stdio://` for a
standalone backend, or `app-server --stdio` when using a full CLI:

```sh
/path/to/codex-acp-v2 \
  --codex-arg=-c \
  --codex-arg='model="your-configured-model"'
```

Useful limits are `--max-sessions` (64), `--request-timeout-seconds` (60),
`--interaction-timeout-seconds` (600), and `--max-frame-bytes` (16777216).
Resource exhaustion is an explicit error, not silent loss of protocol events.
Use `--help` for the authoritative CLI options.

The bundled backend is the unmodified upstream Codex `0.153.3` package from
source revision `b1a547b1f73ce86205d9222ac19cff334b3b7a2e`. Its version, release
URLs, sizes, and SHA-256 digests are pinned independently of the adapter version.
The bundled-default path has been exercised with a real Codex `0.153.3`
runtime and a local mock Responses endpoint for initialization, model discovery,
command/file approvals and execution, dynamic callbacks, stdio/native MCP calls,
child execution, durable root/child
replay, provider errors, and cancellation. Earlier compatibility checks also
used reference source `89a4eec6da` and a Codex `0.153.2` CLI. These checks use no
real account authentication or remote model inference. Features vary by backend version,
account, model, operating system, and feature flags: forwarding a supported
method does not override Codex's own eligibility checks. See the
[known bundled dependency advisories](SECURITY.md#bundled-backend-advisories)
before deployment; the upstream runtime is not covered by the adapter's audit.

## Standard ACP surface

| ACP operation | Backend behavior |
| --- | --- |
| `session/new` | Creates a Codex thread; passes cwd, additional directories, and supported MCP declarations. |
| `session/list` | Lists unarchived Codex threads with cursor pagination. |
| `session/resume` | Resumes an idle persisted thread; optional full history replay precedes the response. |
| `session/fork` | Forks an open idle thread; requires the draft fork capability. |
| `session/prompt` | Starts a turn, or steers active foreground work with `expectedTurnId`. |
| `session/cancel` | Interrupts foreground work and cancels pending interactions. |
| `session/close` | Cancels foreground work, cleans verified loaded descendant work and background terminals, and unsubscribes; does not delete history. |
| `session/delete` | Soft-deletes by archival, avoiding Codex's cascading hard deletion. |
| `session/set_config_option` | Applies session-local settings and returns the authoritative replacement option list. |

Prompt responses acknowledge acceptance, not completion. Clients must process
`session/update` independently, including updates arriving before a prompt
response. `state_update` reports `running`, `requires_action`, or `idle`.
Background tool/subagent updates can still arrive after foreground work is idle.
Session event queues preserve per-session order without making another session
wait behind a slow replay. Cancellation admission is independent of queued
configuration reconciliation.

Messages, emitted reasoning, tools, command terminals, file changes, structured
plans, title/activity metadata, and current-context token usage are projected into ACP v2 updates. Stable
item identities make completed messages/output replacement snapshots, not
additional appended chunks. Terminals are display-only: this adapter does not
ask the ACP client to execute Codex commands. A completed exec tool does not mark
its terminal exited when the process remains running in the background.
Terminal input interactions are reported as tool progress, not fabricated output
bytes. Late command completion updates the same terminal when an exit code arrives.

Backend failures and retries produce a standard, readable diagnostic message,
even without Codex extension subscriptions. Notifications, final failures, and
persisted failures replace the same diagnostic entity rather than duplicating it.
Generated inline PNGs, dynamic image/audio output, MCP structured results, and
web result links have native renderable content; no URL or backend-supplied file
is read to manufacture that content. Local-only attachment paths stay references.
File moves retain both paths. Update hunks become properly headed Git patches;
add/delete snapshots use structured operations plus original text, since Codex
does not supply the file modes needed for complete Git creation/deletion headers.

Replay supports `replayFrom: {"type":"start"}`. Omitting `replayFrom` does not
replay history. History is paginated from Codex and never submitted back to the
model as prompt input. Resume requires the stored cwd and rejects active
foreground sessions. A never-used thread may not yet have a durable rollout:
Codex can reject resume until a first turn has been persisted. Ephemeral threads
also cannot be treated as durable storage.

Supported prompts include text, resource links, images/audio with base64 data,
and embedded text or supported media resources. Links are passed as references,
not fetched or read by the adapter. Arbitrary binary resources and remote image
URLs are rejected rather than silently discarded. Prompt data remains ordinary
user input; embedded resources cannot become system/developer instructions.

MCP declarations support stdio, streamable HTTP, and native MCP-over-ACP. All
become thread-local Codex MCP configuration. A native declaration looks like:

```json
{"type":"acp","name":"client_tools","serverId":"my-client-tools"}
```

The client implements standard v2 `mcp/connect`, `mcp/message`, and
`mcp/disconnect`; Codex extension negotiation is not required for this transport.
The adapter translates native MCP envelopes to a token-protected loopback
streamable-HTTP endpoint, with bidirectional requests, responses, errors, and
notifications. Each backend HTTP MCP session, including inherited subagent
connections, gets an independent client connection. Failed session setup and
close release owned connections and listeners; resume establishes fresh leases.
Limits include 16 native server declarations per ACP session, 32 HTTP sessions
per listener, 128 native connections overall, 1 MiB MCP frames, bounded in-flight
requests, and one reverse SSE stream per HTTP MCP session. SSE does not offer
historical event replay IDs; a new MCP initialization gets independent protocol
state. Legacy SSE server declarations are not part of
the pinned v2 surface. There are no ACP v1 filesystem or client terminal-execution
RPCs.

## Configuration scope

Standard configuration selectors expose model, supported reasoning effort,
service tier, approval policy, sandbox presets, and collaboration mode. They
update the thread through `thread/settings/update`, not the global config file.
Responses wait for matching effective-settings notifications, then return the
whole authoritative option list, including model-dependent reasoning choices.
An acknowledged-but-not-yet-effective backend setting is not treated as a
successful local no-op; standard and extension mutations use the same barrier.
Codex's lifecycle responses omit collaboration mode, summary, and personality,
and do not guarantee an initial full-settings notification. Until a mode is
observed, its selector displays `Backend-managed`; selecting Default or Plan
explicitly applies that preset. Same-connection resume preserves previously
observed omitted settings while keeping newer response-covered fields authoritative.

After negotiating the Codex extension, creation/resume/fork requests may use
`_meta.codex.thread` for explicitly accepted backend controls such as model
provider, instructions, permissions, config overrides, and creation-only tools.
Unsupported fields for the requested lifecycle operation are rejected. Do not
put `threadId`, `cwd`, or transport-owned fields into this metadata.

Custom provider definitions supplied only through thread creation config are
not necessarily persisted by Codex. Configure them through process startup
`--codex-arg=-c` overrides, or re-supply the thread metadata when resuming.

Per-prompt controls belong in `_meta.codex.turn`:

```json
{
  "sessionId": "THREAD_ID",
  "prompt": [{"type": "text", "text": "Return a structured result"}],
  "_meta": {
    "codex": {
      "turn": {
        "outputSchema": {
          "type": "object",
          "properties": {"result": {"type": "string"}},
          "required": ["result"],
          "additionalProperties": false
        }
      }
    }
  }
}
```

Accepted per-prompt keys are `outputSchema`, `serviceTierForTurn`,
`additionalContext`, `turnTrigger`, and `cyberAccessProgram`. Backend eligibility
and semantics still apply; notably, additional context is Codex-managed keyed
context, not a promise that the adapter erases it after a turn. These overrides
cannot accompany steering input into an already-running turn.

For live changes use `_codex/request` with `turn/settings/update` and the exact
active `turnId`. For deliberate use of sticky `turn/start` overrides, use that
backend method through the extension rather than pretending they are one-shot
ACP settings. Live model changes require Codex's `step_model_switching` feature.

## Bidirectional Codex extensions

Opt in during ACP initialization using `capabilities._meta.codex`. The legacy
top-level `_meta.codex` location is accepted for compatibility; conflicting
declarations are rejected:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "protocolVersion": 2,
    "info": {"name": "my-client", "version": "0.1.0"},
    "capabilities": {
      "elicitation": {"form": {}, "url": {}},
      "_meta": {
        "codex": {
          "version": 1,
          "events": ["thread/tokenUsage/updated", "turn/started"],
          "serverRequests": true
        }
      }
    }
  }
}
```

`events` accepts exact backend notification names or `"*"`. `serverRequests`
means the client implements response-requiring Codex callbacks. The initialize
response's `capabilities._meta.codex` publishes the supported method lists and host gate.
Inspect those lists rather than assuming every future backend method is allowed.

Optional `rawServerRequests` selects exact backend callback names or `"*"` for
lossless extension handling even when a native ACP interaction exists. It
requires `serverRequests: true`. For example, select
`item/permissions/requestApproval` to choose a subset of requested permissions
and preserve `strictAutoReview`, or `mcpServer/elicitation/request` for a custom
consent UI. Without that explicit preference, representable interactions use
standard ACP permissions/elicitation; richer interactions use the callback
extension when negotiated. Raw callback clients are responsible for displaying
and validating the backend's actual consent semantics.

Client to adapter:

```json
{
  "jsonrpc": "2.0",
  "id": 10,
  "method": "_codex/request",
  "params": {
    "version": 1,
    "sessionId": "THREAD_ID",
    "method": "thread/backgroundTerminals/list",
    "params": {"threadId": "THREAD_ID"}
  }
}
```

The response result is the original backend result, not a second envelope.
Session-scoped requests must target an open session owned by this connection,
and `sessionId` must agree with `params.threadId`. The narrow exception is
read-only descendant history (`thread/read`, `thread/turns/list`, and
`thread/items/list`): `sessionId` identifies the open ACP root and `threadId`
may identify a descendant whose ancestry is verified against backend metadata.
Creation, resume, fork, and
unsubscribe use ACP lifecycle methods so ownership cannot be bypassed.

History-replacing `thread/rollback` and `thread/revert` require the additional
initialization opt-in `sessionReset: true`. The adapter emits
`_codex/sessionReset` with `{version, sessionId, revision, phase, reason}`;
`phase: "start"` tells the client to clear that session's projected transcript,
standard replay updates rebuild it, and `phase: "complete"` closes the boundary.
The mutation response follows successful replay. These operations require idle
foreground work. Ordinary Codex context-window rollover does not clear the ACP
transcript or emit this history-replacement signal.

Adapter to client notifications use `_codex/event` with
`{version, sessionId, method, params}`. Standard projected updates and subscribed
raw notifications can describe the same event: clients should not render both
as separate chat items.

Subagent callbacks are attached to their backend-verified open ACP root. The
backend child thread ID remains unchanged in raw parameters. Child tool activity
uses namespaced IDs across live updates and durable descendant tool/terminal
replay; child messages and turn-state changes do not masquerade as the root's
messages or foreground lifecycle. Parent collaboration/activity items provide
the generic ACP subagent summary. Detailed child conversations remain available
through the verified read-only extension routes. Replay preserves order within
each thread, but the backend does not provide a shared cross-thread history
cursor, so it cannot reconstruct exact live interleaving across root and children.

Adapter to client requests use `_codex/serverRequest` with
`{version, sessionId, requestId, method, params}`. Respond to the ACP request ID
with the backend method's actual result shape, or a JSON-RPC error. The enclosed
`requestId` is a backend correlation value, not the ACP response ID. This path
supports dynamic tool callbacks, advanced forms, externally managed auth, and
attestation when the relevant backend features are configured. The adapter
does not manufacture tool results or tokens.

Advanced startup callbacks need explicit backend capability configuration:

```sh
./target/release/codex-acp-v2 --allow-host-methods \
  --backend-capabilities='{"requestAttestation":true}'
```

The allowed initialization capability keys are validated, experimental APIs are
always enabled, and required backend notifications cannot be opted out. Clients
must negotiate `serverRequests` when advanced callback capabilities are enabled.
Attestation also requires host authority. OpenAI-specific MCP form settings can
be supplied through the backend `extensions` capability; external auth refresh
is activated by its account login mode, not a separate initialization flag.

Unknown methods are rejected. Known operations still require the backend's
experimental/feature/account support. Reviews, queues, rollback, subagent data,
realtime operations, skills/apps/plugins, MCP operations, and richer config are
available according to the negotiated method lists. These are not automatically
rendered controls in a generic ACP client; clients wanting those features must
implement the corresponding extension UI/behavior.

## Consent and host authority

Command/file approvals become ACP permission requests. Selection returns the
exact offered Codex decision, including session-only or persistent policy
amendments. The labels explicitly distinguish these lifetimes. Additional
permissions offer the requested profile for one turn or the session, or no
grant. Unknown responses are errors, never implicit approval.

Ordinary question forms and supported flat MCP forms use ACP elicitation only
when the client advertises it. Answer keys, primitive types, required fields,
enums, numeric/length bounds, and supported multiselect constraints are checked.
Secret questions, richer OpenAI forms, and schemas whose constraints cannot be
preserved and validated (including regex/format constraints) require the Codex
callback extension. Unsupported interactions fail explicitly if that extension
was not negotiated. URL elicitations require client URL support; the reference
backend does not expose a separate URL-completion notification to translate.

Host/account/global-config/filesystem/process operations require the operator's
`--allow-host-methods` flag as well as client extension negotiation. In
particular, `process/spawn` is not protected by the conversation sandbox.
Hard thread deletion also requires the host gate because Codex can delete
descendants. Standard ACP deletion uses archival instead.

This is a local agent for a trusted ACP client, not an isolation boundary between
mutually untrusted tenants. MCP server commands and configured Codex hooks run
in the host environment. Separate OS accounts/containers and Codex homes are
needed for separate trust domains. Never enable host methods for an untrusted
client. Avoid protocol-level trace logging when credentials or sensitive tool
data may appear in traffic. SDK protocol-packet tracing is unconditionally
filtered by this binary; backend stderr remains controlled by Codex.

## Context management

The ACP session stays stable across Codex context-window rollover. The adapter
does not summarize, compact, restore, or resend the model transcript. Codex's
own selected context implementation, notes/history tools, token budgets, and
model/account feature gates remain authoritative. A raw Codex event named
`contextCompaction` is not proof that summarization occurred; newer backend
modes can open a fresh context window instead.

## Development and verification

Read [AGENTS.md](AGENTS.md) before editing. Default tests use deterministic fake
app-server peers and require Python 3 and Git; they do not need credentials,
paid inference, or external network access. Native MCP transport tests use
loopback HTTP in addition to ACP stdio.

```sh
cargo fmt --all --check
cargo check --all-targets --locked
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
```

To repeat the installed-Codex check with an isolated temporary profile and local
mock model endpoint (requires `codex` on PATH, plus Python 3):

```sh
cargo test --test server_protocol --locked installed_codex_supports_real_protocol_catalog_and_session_lifecycle -- --ignored
cargo test --test installed_workflow --locked -- --ignored --nocapture
```

The second command is a Unix-only fixture using harmless POSIX shell commands.
It runs the full deterministic tool workflow through the
installed Codex executable: harmless command execution, file creation, real
approval requests, successful/failed dynamic tools, both stdio and native ACP
MCP invocation, a real child with command/native MCP tools, durable root/child
replay, a rejected provider request, and cancellation of pending inference.
Only the model Responses endpoint and MCP providers are test doubles; the
Codex runtime, tool execution, protocol transports, and stored rollouts are real.
Everything runs in a temporary workspace and Codex profile without machine
credentials. This is not a live-model or model-quality test.

To exercise a downloaded native release through the same deep Unix workflow,
without an installed Codex or backend override:

```sh
python3 tests/fixtures/installed_workflow.py /absolute/path/to/downloaded-executable
```

The fixture uses a fresh runtime cache and rejects accidental Codex PATH fallback.
Release CI additionally runs a cross-platform default-launch smoke test on each
native target, including Windows.

### Six-part acceptance contract

| Original requirement | Implemented contract and capability boundary | Executable verification |
| --- | --- | --- |
| 1. Codex owns the runtime | Separate backend initialize/initialized handshake; one child process owns inference, tools, sandbox, MCP, subagents, and storage. Adapter history is only a client projection. | `cargo test --test backend_transport --locked` checks independent response/callback dispatch, cancellation cleanup, and explicit bounded-transport failures. Both opt-in installed-Codex commands above exercise the real runtime. |
| 2. Actual ACP v2 Rust surface | Pinned SDK/schema, `unstable_protocol_v2`, `Agent.v2()`, typed `schema::v2` handlers/connections; asynchronous prompt acceptance and native MCP. Optional draft fork support is explicitly advertised. | `cargo check --all-targets --locked`; `cargo test --test server_protocol --locked` drives v2 envelopes through the built binary, not direct mocked handler calls. |
| 3. Standard surfaces | Native new/list/resume/fork/prompt/steer/cancel/close/delete/config; precise permission and supported form/URL interactions; stable messages, reasoning, plans, tools, terminals, title, usage, and descendant tool replay. Delete archives; hard deletion requires host authority. | `cargo test --test server_protocol --test interactions --test projection --locked`: lifecycle/replay/cancel races, rendering identity, consent lifetimes, form constraints/metadata, file moves/Git patches, native errors and rich content. The installed workflow verifies actual tools and durable descendant replay. |
| 4. Configuration scopes | Standard selectors update thread defaults and wait for effective settings. Prompt metadata carries only accepted per-prompt controls; live settings require an exact active turn ID. Sticky raw overrides stay deliberate. No implicit global config writes. | `queued_extension_settings_are_reconciled_before_native_configuration`, `resume_reconciles_full_settings_with_partial_lifecycle_responses`, `standard_lifecycle_streams_approvals_cancels_and_replays_without_feeding_history_back`, and `extensions_are_negotiated_bidirectional_and_share_authoritative_session_state` in `server_protocol`. |
| 5. Bidirectional extension interface | Negotiated allowlisted requests/events/callbacks preserve structured results/errors. Capability metadata is canonical; explicit raw-callback preference and session-reset opt-ins; owned thread/descendant/stream routing; host/account/process operations remain separately gated. Advanced backend families use this lossless interface, not invented generic ACP controls. | `cargo test --test advanced_callbacks --test backend_extensions --locked` covers raw subset grants, strict review, rich form metadata, dynamic errors, auth/attestation gates, capability conflicts, and host/cross-session policy. `server_protocol` checks shared state, stream ownership/cleanup, and history-reset replay boundaries. |
| 6. Execution, streaming, context | Independent bounded event delivery, per-session ordering, replacement snapshots versus appended chunks, display-only terminal output, accurate cancellation/completion, MCP transport bridging, stable sessions across backend context resets. No adapter-authored summaries or model transcript reconstruction. | `cargo test --test native_mcp --test projection --test context_rollover --locked`; `slow_replay_does_not_block_another_sessions_events_or_prompt` in `server_protocol`; installed workflow verifies actual RMCP native transport and tool/callback execution. |

The account/attestation fixtures verify routing, consent, and exact data/error
preservation, not real account login or upstream attestation issuance. Backend
feature eligibility, realtime service availability, model behavior, and large
token-budget context algorithms remain Codex responsibilities; enabling an
extension does not grant those capabilities. The context test verifies the
adapter's stable-session/no-reinjection invariant using actual notification
shapes, without consuming a large model token budget. Live-model checks must
always be explicit opt-ins.

The main modules are `backend` (child RPC transport), `server` (ACP lifecycle and
event routing), `config`, `extensions`, `input`, `projection`, `interactions`,
and `mcp` (session-owned native transport bridge).
The Codex repository is a read-only implementation reference, not a build-time
path dependency.

See [CONTRIBUTING.md](CONTRIBUTING.md) for CI, dependency updates, and the release
process; [CHANGELOG.md](CHANGELOG.md) for changes; and [SECURITY.md](SECURITY.md)
for private vulnerability reporting and trust boundaries.

## License

Licensed under the [Apache License, Version 2.0](LICENSE). Third-party
dependencies retain their respective licenses. Release executables embed their
license and attribution notices; inspect them with `--extract-runtime` and see
[licenses/README.md](licenses/README.md)
for how those records are generated and maintained.
