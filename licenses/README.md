# Third-party release notices

Release archives contain the project's LICENSE and these dependency records:

- `THIRD-PARTY-NOTICES.html` is generated from the locked Cargo dependency graph
  by cargo-about 0.9.2 using the reviewed `about.toml` policy. It includes the
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
the notice files when redistributing release archives. Codex is launched as a
separate executable and is not redistributed in this project's archives.
