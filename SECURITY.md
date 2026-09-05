# Security policy

## Report a vulnerability privately

Use [GitHub private vulnerability reporting](https://github.com/agentprism/codex-acp-v2/security/advisories/new).
Do not report exploitable vulnerabilities in public issues or pull requests.
Include the adapter version, Codex version, operating system, relevant client
capabilities, a minimal reproduction, and the expected versus actual authority
boundary. Redact credentials, account identifiers, private file paths, prompts,
tool output, and tokens. Use synthetic data whenever possible.

The latest released version and the default branch are the supported reporting
targets. There is no promised response deadline or long-term maintenance window
for older versions. Please coordinate disclosure with maintainers so a fix can
be prepared before exploit details become public.

## Trust model

This is a local stdio agent for a trusted ACP v2 client. It launches Codex with
the operator's account, configuration, and OS identity. It is not a sandbox, a
multi-tenant service, or an authentication layer in front of Codex. Use separate
OS accounts or containers and separate Codex profiles for different trust domains.

Binary releases include an unmodified, version-pinned Codex app-server and its
runtime helpers inside each native executable. No backend is downloaded during
startup. The default extracts into a private, payload-hash-keyed per-user cache,
not a `codex` command on PATH. Installation is staged and atomically published;
cached file hashes, sizes, executable modes where supported, and paths are checked
before reuse. Unix caches enforce current-user ownership and private permissions;
Windows caches rely on the user's inherited LocalAppData ACLs. A custom cache
must be private to the operator, especially on Windows, where the adapter does
not rewrite ACLs. Symlinks and Windows reparse points are rejected. A missing
or invalid payload fails explicitly, and corrupt caches are not automatically
deleted. Explicit backend overrides transfer version, provenance, dependency,
and update management to the operator. Keep extracted
executables and helpers in directories writable only by trusted users.

Bundling is distribution, not a new privilege boundary. Existing Codex profiles
and configured hooks/providers retain their normal behavior. The app-server has
no interactive CLI login command; account/login extensions remain subject to
both client negotiation and explicit host authority.

Codex controls its tools and conversation sandbox. The adapter separately gates
host/account/global-configuration/process extensions behind both client
negotiation and the operator's `--allow-host-methods` flag. Host APIs such as
`process/spawn`, configured hooks, and MCP server commands must not be assumed
to run inside a conversation's sandbox. Never enable host methods for an
untrusted client.

Native MCP-over-ACP uses private, token-protected loopback HTTP endpoints.
Endpoint URLs are capabilities: do not publish or log them. Permission and
elicitation responses are security decisions, not UI acknowledgements. Clients
implementing raw Codex callbacks must preserve and display their true scope.

Protocol traffic can contain credentials and sensitive workspace content.
Do not attach raw protocol logs or a Codex home directory to a public report.
The adapter filters SDK packet tracing; backend stderr is controlled by Codex.

## Release verification

GitHub releases include SHA-256 checksums and GitHub build-provenance
attestations. Verify the downloaded native executable before running it using
the commands in [README.md](README.md#install-a-release). Our adapter executables
are not currently signed with Apple Developer ID or Windows Authenticode, and
our macOS adapter is not notarized. These platform signatures are separate from GitHub
provenance attestations.

Verification covers the complete executable, including its embedded upstream package,
helpers, notices, and source references. Upstream executables may retain their
own signatures, but this project's adapter executable does not thereby
gain an Apple or Microsoft platform signature. When redistributing, preserve
notices and access to the matching corresponding-source ZIP release asset.
`--extract-runtime` exposes the embedded notices and metadata for inspection
without launching ACP or Codex. `CODEX_ACP_CACHE_DIR` must name a private absolute
cache path, never a directory writable by untrusted users. Cache checks do not
protect a process against another malicious process with the same OS identity.

Dependency and workflow updates undergo the same required checks as code
changes. Automated advisory and secret scans reduce risk but cannot guarantee
the absence of vulnerabilities or secret material.

The adapter's Cargo.lock audit does not cover the separately built Codex runtime,
its V8/C dependencies, or downloaded helper executables. A backend update needs
its own upstream advisory and dependency review; do not treat the adapter's
green dependency check as a clean bill of health for every bundled component.

## Bundled backend advisories

Codex **0.153.3**, source `b1a547b1f73ce86205d9222ac19cff334b3b7a2e`, includes
`hickory-proto 0.25.2` through its network proxy's Rama DNS dependencies.
[RUSTSEC-2026-0119](https://rustsec.org/advisories/RUSTSEC-2026-0119.html)
identifies excessive CPU work when encoding DNS messages containing many records;
the fix is in Hickory 0.26.1. The pinned upstream
[advisory policy](https://github.com/openai/codex/blob/b1a547b1f73ce86205d9222ac19cff334b3b7a2e/codex-rs/deny.toml#L83)
accepts this dependency pending a Rama update. This release preserves the
official backend unchanged; bundling does not fix that dependency.

Codex uses this dependency as a DNS resolver, not a general DNS server. Our
bounded source review did not establish a path that re-encodes attacker-supplied
large responses in that resolver, but this is not proof of non-exploitability.
Do not treat the backend as advisory-free or expose this local adapter as an
untrusted network service. Review this exposure for your deployment and update
the bundled pin when an appropriate upstream release resolves it.

Other affected dependency versions occur in the shipped build graph. The
following source/feature findings limit our current assessment; they are not
dependency fixes or a guarantee against other call paths:

| Advisory | Bundled dependency | Review finding |
| --- | --- | --- |
| [RUSTSEC-2026-0118](https://rustsec.org/advisories/RUSTSEC-2026-0118.html) | `hickory-proto 0.25.2` | The DNSSEC features required by the reported failure are disabled across the four release targets. |
| [RUSTSEC-2026-0221](https://rustsec.org/advisories/RUSTSEC-2026-0221.html) | `event-listener 5.4.1` | The advisory requires non-`Send` event tags. Reviewed consumers use unit tags; no consuming `Event::with_tag` calls were found. Fixed upstream in 5.4.2. |
| [RUSTSEC-2026-0186](https://rustsec.org/advisories/RUSTSEC-2026-0186.html) | `memmap2 0.9.10` | No calls from reviewed consuming sources to the affected range-advice/flush APIs were found. Fixed upstream in 0.9.11. |

The complete upstream workspace lock also contains development/UI packages not
in the released app-server/helper graph. A workspace-wide audit result is not
equivalent to a shipped-runtime inventory. Conversely, a scoped Rust report does
not audit V8, native helper code, OS libraries, or configured external tools.
Source assembly emits a report-only `backend-audit-report.json` CI artifact,
retaining version-based findings rather than silently suppressing them. Review
that artifact together with the feature and call-site caveats above; the
adapter's own strict audit remains a separate required check.
