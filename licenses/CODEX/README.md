# Bundled Codex notices and corresponding source

This directory accompanies the unmodified upstream Codex app-server **0.153.3**
packages from `openai/codex`, tag `rust-v0.153.3`, source commit
`b1a547b1f73ce86205d9222ac19cff334b3b7a2e`. The adapter's Apache-2.0 license does
not replace the licenses of these separately distributed components.

## Notice inventory

`MANIFEST.json` records every required file here and its SHA-256 digest. Release
packaging verifies the inventory instead of silently omitting missing notices.

| Files | Component and source |
| --- | --- |
| `LICENSE`, `NOTICE` | Unchanged root files from the pinned Codex source. Includes the upstream Ratatui attribution. |
| `APP-SERVER-NOTICES.html`, `CODE-MODE-HOST-NOTICES.html`, `WINDOWS-SANDBOX-NOTICES.html`, `BWRAP-RUST-NOTICES.html` | Original license text and attribution harvested by cargo-about 0.9.2 from each binary crate's pinned Cargo dependency graph across all four release targets. Build dependencies may be included; not every listed crate is linked into every binary. |
| `WEZTERM-LICENSE` | The pinned Codex source's `third_party/wezterm/LICENSE`. |
| `BUBBLEWRAP-LICENSE` | The pinned Codex source's `codex-rs/vendor/bubblewrap/LICENSE`, LGPL-2.0-or-later. Source and build scripts are in the accompanying Codex source. |
| `LIBCAP-LICENSE`, `LIBCAP-COPYRIGHT.txt` | libcap 2.75's original license and its source copyright notices; the BSD-3-Clause option is used. Its source tarball is in the corresponding-source asset. |
| `V8/` | Rusty V8 150.4.0, V8, and native third-party notices omitted from the Cargo crate's packaged license files. Exact source URLs and digests are in `V8/SOURCES.json`. |
| `RIPGREP-NOTICES.html` | ripgrep 15.2.0 and its Cargo dependencies, including the `pcre2` feature used by upstream release builds. The generation uses its release Cargo.lock, not Codex's lockfile. |
| `PCRE2-LICENCE.md`, `JEMALLOC-COPYING` | Native PCRE2 10.45, its JIT/SLJIT support, and jemalloc notices in addition to the Rust wrapper licenses. |
| `ZSH-LICENCE` | Zsh commit `77045ef899e53b9598bebc5a41db93a548a40ca6`, with Codex's execution-wrapper patch. Upstream packages use `codex-zsh-v0.1.0`. Only the core shell executable is distributed, not optional shell-function files. |
| `RUST-STD-NOTICES.html`, `RUST-STD-LICENSES/` | Codex's Rust 1.95.0 compiler standard-library inventory and original license files. |
| `RIPGREP-RUST-NIGHTLY/` | ripgrep's macOS, Windows, and Linux x86-64 standard-library notices from rustc 1.99.0-nightly, `da80ed0708a09dc096c184345d6eb42cbcd50a1e`, distribution date 2026-07-15. |
| `RIPGREP-RUST-STABLE/` | ripgrep's Linux ARM64 standard-library notices from Rust 1.97.0, `2d8144b7880597b6e6d3dfd63a9a9efae3f533d3`. |

`../MUSL-COPYRIGHT.txt` supplies musl's original copyright and license inventory
for statically linked Linux components. Host-provided shared libraries are not
included in these archives. In particular, packaged zsh uses glibc and libtinfo
on Linux; the ARM64 ripgrep also uses host glibc and libgcc_s.

Rusty V8's root source is `denoland/rusty_v8` at
`5c15a6995c9bb4bacd3e341b59fff32c909c80bf` (tag `v150.4.0`). Its V8 submodule is
`denoland/v8` at `ac1e23989121713ca642f6650b34deff7b686896`. The inventory preserves
the LGPL-2.1 license for V8's glibc-derived math code, not merely Rusty V8's MIT
license. The complete vendored source is provided for rebuilding/relinking.
PartitionAlloc's split-repository commit
`ff3b8b885b8374cbd3902642d94dc737bda93d5d` records Chromium origin
`e2ee5821963baed92f29e830f05d257fbfdc6bdd`; its BSD license is copied from that
exact origin. LLVM libraries' dual-license texts and exceptions, ICU data terms,
and other V8 third-party notices remain intact.

Additional notice source references:

- Codex: `https://github.com/openai/codex/tree/b1a547b1f73ce86205d9222ac19cff334b3b7a2e`
- libcap: `https://mirrors.edge.kernel.org/pub/linux/libs/security/linux-privs/libcap2/libcap-2.75.tar.xz`
  (SHA-256 `de4e7e064c9ba451d5234dd46e897d7c71c96a9ebf9a0c445bc04f4742d83632`).
- PCRE2: `https://github.com/PCRE2Project/pcre2/blob/pcre2-10.45/LICENCE.md`.
- jemalloc: `tikv-jemalloc-sys` version
  `0.7.1+5.3.1-0-g81034ce1f1373e37dc865038e1bc8eeecf559ce8`, `jemalloc/COPYING`.
- Zsh: `https://github.com/zsh-users/zsh/blob/77045ef899e53b9598bebc5a41db93a548a40ca6/LICENCE`.
- Rust nightly compiler archive:
  `https://static.rust-lang.org/dist/2026-07-15/rustc-nightly-x86_64-unknown-linux-gnu.tar.xz`
  (SHA-256 `dad49ece98c6d0e5f3bfd7c532b5111f55319a0e2a880ef1a87379643348cf8e`).
- Rust 1.97.0 compiler archive:
  `https://static.rust-lang.org/dist/rustc-1.97.0-x86_64-unknown-linux-gnu.tar.xz`
  (SHA-256 `0a8787303c88b018af61b5c53a0c7024d516d175e623eeab35a35eab11dbcad0`).

## Obtain and rebuild the source

Every embedded runtime carries `codex/SOURCE.tar.gz`, the exact upstream source
archive, SHA-256
`bdd4df80f52e9e831eec6fd892fc5c99cc04dc1214b545c2a2843edc9e43dbbe`.
The same GitHub release also provides **`codex-backend-sources-0.153.3.zip`**:

<https://github.com/agentprism/codex-acp-v2/releases>

Download the source asset from the release that supplied your binary, verify its
entry in `SHA256SUMS` and its GitHub provenance attestation, then extract it.
It contains `upstream/` with the complete Codex source and a versioned
`upstream/codex-rs/vendor-crates/` Cargo vendor tree, including V8's native
sources, MPL-2.0 dependencies, and build scripts. It also contains the original
Codex archive, pinned libcap source, build metadata, and this notice directory.
This is source access for recipients, not an offer limited to maintainers.

The only upstream lockfile normalization changes 149 local workspace package
versions from `0.0.0` to `0.153.3`; the upstream release tag had left those
entries stale. All 1,232 external registry/git packages, checksums, revisions,
and dependency entries remain unchanged. A Cargo source-replacement configuration
is appended to the upstream `.cargo/config.toml`. The original, unmodified source
archive is retained alongside it. Vendor source files are unmodified.

### Bubblewrap helper

An isolated Linux rebuild is verified during source assembly with Rust 1.95.0,
an empty Cargo home, `--offline --locked`, the supplied vendor tree, and the
supplied libcap 2.75 source. This verifies a native GNU/Linux rebuild, not
bit-for-bit reproduction of upstream's musl release or host namespace policy.
You need Rust 1.95.0, a C compiler, GNU make, gperf, pkg-config, and Linux headers.

From the extracted source asset:

```sh
tar -xJf libcap-2.75.tar.xz
make -C libcap-2.75/libcap -j2 libcap.a CC=cc BUILD_CC=cc AR=ar RANLIB=ranlib
```

Create a `libcap.pc` in a temporary pkg-config directory with these fields,
replacing `/absolute/source-root` with this source asset's extracted location:

```pkgconfig
Name: libcap
Description: libcap built from accompanying source
Version: 2.75
Libs: -L/absolute/source-root/libcap-2.75/libcap -lcap
Cflags: -I/absolute/source-root/libcap-2.75/libcap/include
```

Then build and inspect the helper, using that directory's absolute path:

```sh
cd upstream/codex-rs
PKG_CONFIG_PATH=/absolute/pkgconfig-directory PKG_CONFIG_ALL_STATIC=1 \
  cargo +1.95.0 build --offline --locked -p codex-bwrap --bin bwrap
./target/debug/bwrap --help
```

The C source and headers are in `upstream/codex-rs/vendor/bubblewrap/`; the Rust
wrapper, build script, and manifest are in `upstream/codex-rs/bwrap/`.
You can modify and rebuild them and replace `codex/codex-resources/bwrap` in a
copy of the extracted binary package. Upstream's exact release-target compiler
and musl/libcap staging steps are in
`upstream/.github/scripts/install-musl-build-tools.sh` and its release workflow.

### App-server, code-mode host, and V8

Use the upstream package builder and build instructions included in
`upstream/scripts/codex_package/README.md`. Rust 1.95.0 and the appropriate C/C++
compiler, SDK, linker, GN, Ninja, Python, and other platform build tools remain
build prerequisites. For a source rebuild of V8, set `V8_FROM_SOURCE=1`, do not
set `RUSTY_V8_ARCHIVE` or `RUSTY_V8_SRC_BINDING_PATH`, and build the actual
app-server and code-mode host from `upstream/codex-rs`:

```sh
V8_FROM_SOURCE=1 cargo +1.95.0 build --offline --locked --release \
  --bin codex-app-server --bin codex-code-mode-host
```

Cargo dependencies are available offline; native build scripts can still need
build-tool downloads unless you supply their required tools. Rusty V8's
`build.rs` supports explicit `GN`, `NINJA`, and `CLANG_BASE_PATH` overrides.
The glibc-derived math implementation and its build definition are in
`vendor-crates/v8-150.4.0/v8/third_party/glibc/`. To modify a checksummed vendored
crate, copy it to a local development directory and use an appropriate Cargo
path patch; otherwise Cargo correctly rejects changed vendor checksums. Rebuild
and relink the dependent executables against that modified crate. Explicit
`--app-server-path` lets the adapter run your rebuilt standalone backend, keeping
its matching runtime resources together.

The adapter imposes no additional prohibition on modification, rebuilding,
relinking, or reverse engineering to debug modifications to LGPL components.
Redistribution remains subject to each component's license and notices. No
claim is made that a full V8/backend source build is bit-identical to upstream's
signed binaries; the bounded offline check covers bubblewrap.

## Refreshing this inventory

Update backend artifacts, source and helper pins, notices, source material, and
this inventory together. The backend policy in `about.toml` is separate from
the adapter's root policy. After the verified local-version normalization,
run cargo-about 0.9.2 with `--locked --fail`, this `about.toml` and `about.hbs`,
once for each upstream manifest: `app-server`, `code-mode-host`,
`windows-sandbox-rs`, and `bwrap`. For ripgrep, use its 15.2.0 release source,
`--features pcre2`, and its own unchanged Cargo.lock. The HTML inventories retain
upstream license text and attribution, not just SPDX identifiers.

Re-harvest V8 files from the exact URLs in `V8/SOURCES.json`; confirm the submodule
revisions against Rusty V8's pinned `.gitmodules` and tree before accepting a
new V8 release. Copy the standard-library inventory and license directory from
each exact compiler distribution identified above. Recompute `MANIFEST.json`
only after reviewing changed licenses and source obligations. The broad source
vendor tree deliberately includes workspace/build-only packages for complete
build context; their presence does not imply they are shipped in each binary.
