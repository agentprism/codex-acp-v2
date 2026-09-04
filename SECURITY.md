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
attestations. Verify the downloaded archive before running its executable using
the commands in [README.md](README.md#install-a-release). Binaries are not
currently signed with Apple Developer ID or Windows Authenticode, and macOS
binaries are not notarized. These platform signatures are separate from GitHub
provenance attestations.

Dependency and workflow updates undergo the same required checks as code
changes. Automated advisory and secret scans reduce risk but cannot guarantee
the absence of vulnerabilities or secret material.
