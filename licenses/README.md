# Third-party release notices

Release executables embed the project's LICENSE and these dependency records.
Run `codex-acp-v2 --extract-runtime` to inspect them without starting the agent:

- `THIRD-PARTY-NOTICES.html` is generated from the locked Cargo dependency graph
  by cargo-about 0.9.2 with all features using the reviewed `about.toml` policy.
  It includes the
  original harvested license text and attribution where available, including
  dependencies with conjunctive BSD and Unicode terms.
- `RUST-STD-NOTICES.html` and `RUST-STD-LICENSES/` are copied from the exact
  release compiler's `share/doc/rust/COPYRIGHT-library.html` and `licenses/`.
  These cover standard-library source and its dependencies, which do not appear
  in the application's Cargo.lock. This upstream inventory is intentionally
  broader than the code linked into any one executable.
- `licenses/MUSL-COPYRIGHT.txt` is the unmodified musl 1.2.5 COPYRIGHT file from
  https://git.musl-libc.org/cgit/musl/plain/COPYRIGHT?h=v1.2.5. It applies to the
  statically linked Linux release targets. The pinned Rust 1.98.1 toolchain
  builds musl 1.2.5 with security patches, as recorded in
  https://github.com/rust-lang/rust/blob/1.98.1/src/ci/docker/scripts/musl-toolchain.sh.

When changing the compiler, targets, or dependency graph, regenerate the Cargo
notices and review whether these non-Cargo notices also need an update. Preserve
the embedded notice files when redistributing release executables.

Releases from 0.2.0 also redistribute the complete upstream Codex app-server
package. Its independent license inventory and source/rebuild information are
under [CODEX/README.md](CODEX/README.md). Those notices cover Codex, its Rust
dependencies, V8 and native components, ripgrep, and platform helpers; they are
not generated from the adapter's Cargo.lock and do not change the adapter's
Apache-2.0 license. Preserve access to the corresponding-source asset published
alongside the native executables when redistributing them. That separate
corresponding-source ZIP is not an installation package.
