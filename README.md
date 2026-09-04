# Codex ACP v2

A Rust ACP **protocol v2** agent that delegates execution to a Codex app-server
child. Codex owns inference, tools, sandboxing, MCP, subagents, model history, and
context management. This adapter owns ACP sessions, configuration translation,
client-visible history, interaction routing, and negotiated Codex extensions.

This uses `Agent.v2()` and `schema::v2`, with the SDK's `unstable_protocol_v2`
feature. It is not a protocol-v1 adapter merely using a version-2 Rust package.
The draft SDK is pinned to `agent-client-protocol = 2.1.0`; `Cargo.lock` pins the
schema to `1.7.0`. Rust `1.98.1` and edition 2024 are pinned in the repository.

## Build and run

Install Rust using rustup, and install a Codex executable compatible with the
app-server protocol referenced by this project. Authenticate/configure Codex
separately, for example with `codex login`. The adapter uses that executable's
normal account, configuration, and execution environment.

```sh
cargo build --release --locked
./target/release/codex-acp-v2 --codex-path /absolute/path/to/codex
```

Configure your ACP v2 client to launch that executable on stdin/stdout. Stdout is
reserved for JSON-RPC traffic; diagnostics and child diagnostics use stderr.
There is no TCP listener and no shared multi-tenant daemon. Each adapter process
owns one app-server process and can host multiple threads.

`--codex-path` defaults to `codex` and can also be supplied through `CODEX_PATH`.
Repeat `--codex-arg` for arguments that precede `app-server --stdio`:

```sh
./target/release/codex-acp-v2 \
  --codex-arg=-c \
  --codex-arg='model="your-configured-model"'
```

Useful limits are `--max-sessions` (64), `--request-timeout-seconds` (60),
`--interaction-timeout-seconds` (600), and `--max-frame-bytes` (16777216).
Resource exhaustion is an explicit error, not silent loss of protocol events.
Use `--help` for the authoritative CLI options.

The reference backend is the Codex source at revision `89a4eec6da`. A real
Codex `0.153.2` executable has also been exercised against a local mock Responses
endpoint for initialization, model discovery, real backend turn execution,
assistant/usage/completion events, and durable close/resume. That check uses no
real account authentication or remote model inference. Features vary by backend version,
account, model, operating system, and feature flags: forwarding a supported
method does not override Codex's own eligibility checks.

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

Messages, emitted reasoning, tools, command terminals, file diffs, structured
plans, and current-context token usage are projected into ACP v2 updates. Stable
item identities make completed messages/output replacement snapshots, not
additional appended chunks. Terminals are display-only: this adapter does not
ask the ACP client to execute Codex commands. A completed exec tool does not mark
its terminal exited when the process remains running in the background.

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

MCP declarations support stdio and streamable HTTP. They become thread-local
Codex MCP configuration. Native MCP-over-ACP and legacy SSE are not advertised.
There are no ACP v1 filesystem or client terminal-execution RPCs.

## Configuration scope

Standard configuration selectors expose model, supported reasoning effort,
service tier, approval policy, sandbox presets, and collaboration mode. They
update the thread through `thread/settings/update`, not the global config file.
Backend effective-settings notifications reconcile the client's configuration.

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

Opt in during ACP initialization using the request's top-level `_meta`:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "initialize",
  "params": {
    "protocolVersion": 2,
    "info": {"name": "my-client", "version": "0.1.0"},
    "capabilities": {"elicitation": {"form": {}, "url": {}}},
    "_meta": {
      "codex": {
        "version": 1,
        "events": ["thread/tokenUsage/updated", "turn/started"],
        "serverRequests": true
      }
    }
  }
}
```

`events` accepts exact backend notification names or `"*"`. `serverRequests`
means the client implements response-requiring Codex callbacks. The initialize
response's `_meta.codex` publishes the supported method lists and host gate.
Inspect those lists rather than assuming every future backend method is allowed.

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

Adapter to client notifications use `_codex/event` with
`{version, sessionId, method, params}`. Standard projected updates and subscribed
raw notifications can describe the same event: clients should not render both
as separate chat items.

Subagent callbacks are attached to their backend-verified open ACP root. The
backend child thread ID remains unchanged in raw parameters. Child tool activity
uses namespaced IDs; child messages and turn-state changes do not masquerade as
the root's messages or foreground lifecycle. Parent collaboration/activity items
provide the generic ACP subagent view; richer child history is available through
the verified read-only extension routes.

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
app-server peers and require `python3`; they do not need credentials, paid
inference, or external network access.

```sh
cargo fmt --all --check
cargo check --all-targets --locked
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
```

To repeat the installed-Codex check with an isolated temporary profile and local
mock model endpoint (requires `codex` on PATH, plus Python 3):

```sh
cargo test --test server_protocol installed_codex_supports_real_protocol_catalog_and_session_lifecycle -- --ignored
```

Tests target meaningful risks: wire ordering, callback resolution, cancellation,
replay identity, configuration scope, extension authorization, transport limits,
and child shutdown. Mock tests prove adapter behavior, not model quality or all
possible backend/account integrations. Live-model checks must be explicit opt-ins.

The main modules are `backend` (child RPC transport), `server` (ACP lifecycle and
event routing), `config`, `extensions`, `input`, `projection`, and `interactions`.
The Codex repository is a read-only implementation reference, not a build-time
path dependency.
