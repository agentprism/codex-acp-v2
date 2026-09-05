"""Assemble pinned corresponding sources and prove the bwrap helper rebuilds offline."""

import argparse
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import stat
import tarfile
import tempfile
import tomllib
import zipfile

import archives
import backend


def normalize_local_versions(text, version):
    """The upstream release bump leaves only workspace package versions stale."""
    before = tomllib.loads(text)
    sections = text.split("[[package]]")
    for index, section in enumerate(sections[1:], 1):
        package = tomllib.loads("[[package]]" + section)["package"][0]
        if "source" not in package:
            if package["version"] != "0.0.0":
                raise ValueError("upstream local lock versions changed; review source normalization")
            sections[index], count = re.subn(r'^version = "0\.0\.0"$', f'version = "{version}"',
                                            section, count=1, flags=re.MULTILINE)
            if count != 1:
                raise ValueError("could not normalize one local workspace version")
    normalized = "[[package]]".join(sections)
    after = tomllib.loads(normalized)
    expected = json.loads(json.dumps(before))
    for package in expected["package"]:
        if "source" not in package:
            package["version"] = version
    if after != expected:
        raise ValueError("normalization changed dependencies or non-version lockfile fields")
    return normalized


def extract_sources(path, destination):
    """Use Python's data filter after bounded inspection, allowing safe source symlinks."""
    with tarfile.open(path) as archive:
        members = archive.getmembers()
        if len(members) > 20000 or sum(entry.size for entry in members) > 256 * 1024 * 1024:
            raise ValueError("upstream source archive exceeds extraction bounds")
        if any(entry.size < 0 or entry.size > 32 * 1024 * 1024 for entry in members):
            raise ValueError("upstream source file exceeds extraction bounds")
        archive.extractall(destination, filter="data")


def rebuild_bwrap(upstream, libcap_archive, scratch, environment):
    extract_sources(libcap_archive, scratch)
    libcap = scratch / "libcap-2.75"
    subprocess.run(["make", "-C", str(libcap / "libcap"), "-j2", "libcap.a",
                    "CC=cc", "BUILD_CC=cc", "AR=ar", "RANLIB=ranlib"], check=True, timeout=300)
    pkgconfig = scratch / "pkgconfig"
    pkgconfig.mkdir()
    (pkgconfig / "libcap.pc").write_text(
        f"Name: libcap\nDescription: pinned libcap\nVersion: 2.75\n"
        f"Libs: -L{libcap / 'libcap'} -lcap\nCflags: -I{libcap / 'libcap/include'}\n",
        encoding="utf-8",
    )
    cargo_home = scratch / "empty-cargo-home"
    cargo_home.mkdir()
    target = scratch / "rebuild-target"
    environment = {**environment, "CARGO_HOME": str(cargo_home), "CARGO_TARGET_DIR": str(target),
                   "PKG_CONFIG_PATH": str(pkgconfig), "PKG_CONFIG_ALL_STATIC": "1"}
    subprocess.run(["cargo", "build", "--offline", "--locked", "-p", "codex-bwrap", "--bin", "bwrap"],
                   cwd=upstream / "codex-rs", env=environment, check=True, timeout=600)
    subprocess.run([str(target / "debug/bwrap"), "--help"], check=True, timeout=30,
                   stdout=subprocess.DEVNULL)


def verify_source_archive(archive_path, lock, environment):
    """Read every emitted byte and rebuild from the downloadable ZIP, not its inputs."""
    with tempfile.TemporaryDirectory(prefix="codex-source-verify-", dir=archive_path.parent) as temporary:
        scratch = Path(temporary).resolve()
        with zipfile.ZipFile(archive_path) as archive:
            entries = archive.infolist()
            if len(entries) > 200000 or sum(entry.file_size for entry in entries) > 4 * 1024 ** 3:
                raise ValueError("corresponding sources exceed bounded ZIP extraction limits")
            seen = set()
            for entry in entries:
                path = PurePosixPath(entry.filename)
                if (not stat.S_ISREG(entry.external_attr >> 16) or path.is_absolute()
                        or entry.filename != path.as_posix() or ".." in path.parts
                        or "\\" in entry.filename or ":" in entry.filename or entry.filename in seen
                        or entry.file_size > 512 * 1024 * 1024):
                    raise ValueError(f"unsafe corresponding-source ZIP entry: {entry.filename}")
                seen.add(entry.filename)
            for entry in entries:
                path = scratch / entry.filename
                path.parent.mkdir(parents=True, exist_ok=True)
                with archive.open(entry) as source, path.open("xb") as output:
                    shutil.copyfileobj(source, output, length=1024 * 1024)
                path.chmod(0o755 if entry.external_attr >> 16 & 0o111 else 0o644)
        root = scratch / f"codex-backend-sources-{lock['version']}"
        backend.verify_archive(root / "SOURCE.tar.gz", lock["source"])
        backend.verify_archive(root / "libcap-2.75.tar.xz", lock["rebuild"]["libcap"])
        backend.verify_notices(lock, root / "licenses/CODEX")
        rebuild_bwrap(root / "upstream", root / "libcap-2.75.tar.xz", scratch, environment)


def assemble(args):
    lock = backend.read_lock()
    backend.verify_notices(lock)
    source = args.source_archive or backend.download(
        lock["source"], args.cache / f"codex-source-{lock['commit']}.tar.gz")
    libcap = args.libcap_archive or backend.download(lock["rebuild"]["libcap"], args.cache / "libcap-2.75.tar.xz")
    backend.verify_archive(source, lock["source"])
    backend.verify_archive(libcap, lock["rebuild"]["libcap"])
    environment = {**os.environ, "RUSTUP_TOOLCHAIN": lock["rebuild"]["rustToolchain"]}
    args.cache.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="codex-source-build-", dir=args.cache.parent) as temporary:
        scratch = Path(temporary).resolve()
        extract_sources(source, scratch)
        upstream = scratch / f"codex-{lock['commit']}"
        workspace = upstream / "codex-rs"
        workspace_lock = workspace / "Cargo.lock"
        normalized = normalize_local_versions(workspace_lock.read_text(encoding="utf-8"), lock["version"])
        workspace_lock.write_text(normalized, encoding="utf-8")
        if args.vendor:
            shutil.copytree(args.vendor, workspace / "vendor-crates", copy_function=os.link)
            config = args.vendor_config.read_text(encoding="utf-8")
        else:
            config = subprocess.check_output(
                ["cargo", "vendor", "--locked", "--versioned-dirs", "vendor-crates"],
                cwd=workspace, env=environment, text=True, timeout=1200,
            )
        parsed = tomllib.loads(config)
        for item in parsed["source"].values():
            if "directory" in item:
                config = config.replace(f'directory = "{item["directory"]}"', 'directory = "vendor-crates"')
        # Keep upstream compiler flags, adding only Cargo's generated source replacement.
        cargo_config = workspace / ".cargo/config.toml"
        with cargo_config.open("a", encoding="utf-8") as output:
            output.write("\n" + config)
        subprocess.run(["cargo", "metadata", "--offline", "--locked", "--format-version", "1"],
                       cwd=workspace, env=environment, check=True, timeout=120,
                       stdout=subprocess.DEVNULL)
        rebuild_bwrap(upstream, libcap, scratch, environment)
        info = {"backend": lock, "localLockNormalization": "Only workspace versions 0.0.0 to release version",
                "offlineRebuildVerified": "codex-bwrap with pinned libcap; native Linux, not bit-identical",
                "adapterCommit": subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()}
        files = {
            "SOURCE.tar.gz": (source, 0o644), "libcap-2.75.tar.xz": (libcap, 0o644),
            "backend.lock.json": (Path("backend.lock.json"), 0o644),
            "BUILD-INFO.json": ((json.dumps(info, indent=2) + "\n").encode(), 0o644),
            "README.md": (Path("licenses/CODEX/README.md"), 0o644),
        }
        for prefix, directory in (("upstream", upstream), ("licenses/CODEX", Path("licenses/CODEX"))):
            root = directory.resolve()
            for path in sorted(directory.rglob("*")):
                if path.is_file():
                    if not path.resolve().is_relative_to(root):
                        raise ValueError(f"source symlink escapes its pinned tree: {path}")
                    files[f"{prefix}/{path.relative_to(directory).as_posix()}"] = (
                        path, 0o755 if path.stat().st_mode & 0o111 else 0o644)
                elif not path.is_dir():
                    raise ValueError(f"source contains an unsupported special file: {path}")
        name = f"codex-backend-sources-{lock['version']}"
        destination = Path("dist") / f"{name}.zip"
        archives.write_archive(destination, name, files)
    verify_source_archive(destination, lock, environment)
    print(f"Verified corresponding-source ZIP by complete extraction and offline rebuild: {destination}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cache", type=Path, default=Path("target/backend-downloads"))
    parser.add_argument("--source-archive", type=Path)
    parser.add_argument("--libcap-archive", type=Path)
    parser.add_argument("--vendor", type=Path, help="Reuse a local cargo vendor tree (not used in CI)")
    parser.add_argument("--vendor-config", type=Path)
    args = parser.parse_args()
    if bool(args.vendor) != bool(args.vendor_config):
        parser.error("--vendor and --vendor-config must be supplied together")
    assemble(args)


if __name__ == "__main__":
    main()
