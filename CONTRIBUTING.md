# Contributing

Read [AGENTS.md](AGENTS.md) for architecture, Rust conventions, security
invariants, and conservative test-authoring guidance. Keep changes focused and
describe the user-visible behavior or demonstrated regression they address.
Security reports belong in the [private reporting channel](SECURITY.md), not
public issues.

## Local checks

Install Rust through rustup, Python 3, and Git. The checked-in
`rust-toolchain.toml` selects the supported stable Rust toolchain. Cargo.lock is
committed, and builds use `--locked`. Default tests do not require Codex,
credentials, paid inference, or external network services; some open loopback
listeners and spawn deterministic Python peers.

```sh
cargo fmt --all --check
cargo check --all-targets --locked
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
```

The [installed-Codex checks](README.md#development-and-verification) are explicit
opt-ins. They use isolated temporary profiles and local mock model endpoints;
the deeper Unix-only workflow executes real harmless tools. Passing them does
not mean real account authentication, paid inference, or model quality was tested.

Do not replace portable tests with Unix-only fixtures to make CI pass. Mark a
test's genuine platform requirement explicitly and keep the core protocol
contract covered on Linux, Apple Silicon macOS, and Windows.

## Dependencies and automation

Keep the ACP SDK pinned while protocol v2 is draft. Updating the SDK requires
reviewing the wire schema and protocol semantics, not just compilation. Update
the Rust pin and `package.rust-version` together when moving to a newer stable
toolchain. Commit Cargo.lock changes with the manifest change that requires them.

Review dependency advisories and license changes before accepting updates.
Generate release dependency notices with the pinned tool version:

```sh
cargo install cargo-about --version 0.9.2 --features cli --locked
cargo about generate --locked --fail about.hbs --output-file THIRD-PARTY-NOTICES.html
```

The generated file is a build artifact, not a checked-in snapshot. `about.toml`
covers all release targets, includes transitive dependencies, and fails on
unaccepted license expressions. Changing that policy requires review; do not
silence a newly introduced license requirement. The release also carries Rust
standard-library and musl notices described in [licenses/README.md](licenses/README.md).

CI actions are pinned to immutable commits and updated through dependency
pull requests. Keep workflow permissions minimal, avoid exposing credentials to
untrusted pull-request code, and do not replace a failing security gate with an
unreviewed allowlist entry. Changes to release workflows deserve the same review
as executable code.

## Releases

Releases are GitHub binary releases. `publish = false` intentionally prevents
accidental crates.io publication; this repository is not automatically published
to a package registry.

1. Prepare a focused release change updating `Cargo.toml`, its matching
   Cargo.lock package entry, and [CHANGELOG.md](CHANGELOG.md). Use a SemVer
   version; the tag is the same value prefixed with `v`.
2. Merge only after required checks pass on the default branch. Review the
   resulting changes and ensure the working tree is clean.
3. Create an annotated version tag at that reviewed commit and push only that
   tag. For the initial version:

   ```sh
   git tag -a v0.1.0 -m "Release v0.1.0"
   git push origin v0.1.0
   ```

4. Monitor the Release workflow. It validates the tag against the manifest and
   lockfile, runs checks, builds and smoke-tests the four native target binaries,
   packages documentation/notices/build metadata, and produces SHA256SUMS and
   GitHub provenance attestations. Publication occurs only after all release
   assets are ready. A SemVer prerelease suffix creates a GitHub prerelease.
   If publication fails after uploads, rerun failed jobs to reuse the exact
   original archives; the publisher refuses to replace existing asset bytes.
5. Download the published archives, verify their checksums and provenance, and
   check the release notes and asset list. Never move a published version tag or
   silently replace a released binary. Make a new version for a correction.

The target matrix is Apple Silicon macOS, Linux x86-64 and ARM64 (static musl),
and Windows x86-64 (MSVC with a statically linked CRT). Codex is a separately installed runtime prerequisite,
not an embedded component of these archives. OS code signing and notarization
require separate publisher identities and credentials; the current workflow
does not claim to perform either.
