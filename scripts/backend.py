"""Stage a complete, digest-pinned upstream app-server package; never download at runtime."""

import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import tarfile
import tempfile
import time
import urllib.error
import urllib.request


TARGETS = (
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-musl",
)
MAX_ARCHIVE_BYTES = 256 * 1024 * 1024
MAX_FILE_BYTES = 384 * 1024 * 1024
MAX_EXPANDED_BYTES = 768 * 1024 * 1024
MAX_MEMBERS = 128


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def required_files(target):
    suffix = ".exe" if target.endswith("windows-msvc") else ""
    paths = {
        "codex-package.json", f"bin/codex-app-server{suffix}",
        f"bin/codex-code-mode-host{suffix}", f"codex-path/rg{suffix}",
    }
    if suffix:
        paths.update(("codex-resources/codex-command-runner.exe",
                      "codex-resources/codex-windows-sandbox-setup.exe"))
    else:
        paths.add("codex-resources/zsh/bin/zsh")
    if "linux" in target:
        paths.add("codex-resources/bwrap")
    return paths


def read_lock(path=Path("backend.lock.json")):
    lock = json.loads(path.read_text(encoding="utf-8"))
    if lock.get("schemaVersion") != 1 or lock.get("repository") != "https://github.com/openai/codex":
        raise ValueError("unsupported backend lock schema or repository")
    if not re.fullmatch(r"\d+\.\d+\.\d+", lock["version"]) or lock["tag"] != f"rust-v{lock['version']}":
        raise ValueError("backend lock must identify one exact stable Codex release")
    if not re.fullmatch(r"[0-9a-f]{40}", lock["commit"]) or set(lock["packages"]) != set(TARGETS):
        raise ValueError("backend lock must pin the full source commit and complete target matrix")
    for target, item in lock["packages"].items():
        expected = f"https://github.com/openai/codex/releases/download/{lock['tag']}/codex-app-server-package-{target}.tar.gz"
        if item["url"] != expected:
            raise ValueError("backend package URL does not match its pinned release and target")
    if lock["source"]["url"] != f"https://codeload.github.com/openai/codex/tar.gz/{lock['commit']}":
        raise ValueError("backend source URL does not match its pinned commit")
    if not re.fullmatch(r"\d+\.\d+\.\d+", lock["rebuild"]["rustToolchain"]):
        raise ValueError("backend rebuild toolchain must be pinned")
    if lock["rebuild"]["libcap"]["url"] != "https://mirrors.edge.kernel.org/pub/linux/libs/security/linux-privs/libcap2/libcap-2.75.tar.xz":
        raise ValueError("backend rebuild requires the reviewed upstream libcap source")
    for item in (*lock["packages"].values(), lock["source"], lock["rebuild"]["libcap"]):
        if type(item["size"]) is not int or not 0 < item["size"] <= MAX_ARCHIVE_BYTES:
            raise ValueError("backend archive exceeds the permitted download size")
        if not re.fullmatch(r"[0-9a-f]{64}", item["sha256"]):
            raise ValueError("backend archives require a full SHA-256 digest")
    return lock


def verify_archive(path, specification):
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"backend archive is not a regular file: {path}")
    if path.stat().st_size != specification["size"] or sha256(path) != specification["sha256"]:
        raise ValueError(f"backend archive does not match its pinned size and SHA-256: {path}")


def download(specification, path):
    if path.exists():
        verify_archive(path, specification)
        return path
    path.parent.mkdir(parents=True, exist_ok=True)
    for attempt in range(3):
        temporary = None
        try:
            request = urllib.request.Request(specification["url"], headers={"User-Agent": "codex-acp-v2-release"})
            with urllib.request.urlopen(request, timeout=30) as response:
                with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as output:
                    temporary = Path(output.name)
                    received = 0
                    while chunk := response.read(1024 * 1024):
                        received += len(chunk)
                        if received > specification["size"]:
                            raise ValueError("backend download exceeds its pinned size")
                        output.write(chunk)
            verify_archive(temporary, specification)
            temporary.replace(path)
            return path
        except (urllib.error.URLError, TimeoutError):
            if attempt == 2:
                raise
            time.sleep(attempt + 1)
        finally:
            if temporary is not None:
                temporary.unlink(missing_ok=True)
    raise RuntimeError("backend download did not complete")


def metadata(lock, target):
    suffix = ".exe" if target.endswith("windows-msvc") else ""
    return {
        "layoutVersion": 1, "version": lock["version"], "target": target,
        "variant": "codex-app-server", "entrypoint": f"bin/codex-app-server{suffix}",
        "resourcesDir": "codex-resources", "pathDir": "codex-path",
    }


def package_members(archive, lock, target):
    """Validate the whole tar before extraction, including platform path ambiguity."""
    members = []
    seen = set()
    total = 0
    expected = required_files(target)
    for member in archive:
        if len(members) >= MAX_MEMBERS:
            raise ValueError("backend package has too many members")
        name = member.name.rstrip("/")
        path = PurePosixPath(name)
        if (not name or name != path.as_posix() or path.is_absolute()
                or ".." in path.parts or "\\" in name or ":" in name
                or name.casefold() in seen):
            raise ValueError(f"unsafe or duplicate backend archive path: {member.name}")
        seen.add(name.casefold())
        if not (member.isfile() or member.isdir()):
            raise ValueError(f"backend archive links and special files are forbidden: {name}")
        if member.size < 0 or member.size > MAX_FILE_BYTES:
            raise ValueError("backend archive member exceeds its size limit")
        total += member.size
        if total > MAX_EXPANDED_BYTES:
            raise ValueError("backend package exceeds its expanded size limit")
        if member.isfile() and name not in expected:
            raise ValueError(f"unexpected backend package file: {name}")
        if member.isdir() and not any(candidate.startswith(name + "/") for candidate in expected):
            raise ValueError(f"unexpected backend package directory: {name}")
        if (member.isfile() and name != "codex-package.json"
                and not target.endswith("windows-msvc") and not member.mode & 0o111):
            raise ValueError(f"backend helper is not executable: {name}")
        members.append(member)
    if {item.name for item in members if item.isfile()} != expected:
        raise ValueError("backend package is missing required executables or resources")
    manifest = archive.getmember("codex-package.json")
    if manifest.size > 4096:
        raise ValueError("backend package metadata exceeds its size limit")
    with archive.extractfile(manifest) as stream:
        actual = json.load(stream)
    if actual != metadata(lock, target):
        raise ValueError("backend package metadata disagrees with its pinned version or target")
    return members


def bundle_files(directory, lock, target):
    """Verify a staged package and its source archive before it enters a release."""
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError("bundled backend must be a real directory")
    expected = required_files(target) | {"SOURCE.tar.gz"}
    paths = {}
    for path in directory.rglob("*"):
        if path.is_symlink() or not (path.is_dir() or path.is_file()):
            raise ValueError(f"bundled backend contains a link or special file: {path}")
        if path.is_file():
            paths[path.relative_to(directory).as_posix()] = path
    if set(paths) != expected:
        raise ValueError("bundled backend has missing or unexpected package files")
    if json.loads(paths["codex-package.json"].read_text(encoding="utf-8")) != metadata(lock, target):
        raise ValueError("bundled backend metadata disagrees with its lockfile")
    verify_archive(paths["SOURCE.tar.gz"], lock["source"])
    return paths


def verified_files(directory, archive_path, lock, target):
    paths = bundle_files(directory, lock, target)
    verify_archive(archive_path, lock["packages"][target])
    with tarfile.open(archive_path, "r:gz") as archive:
        for member in package_members(archive, lock, target):
            if not member.isfile():
                continue
            with archive.extractfile(member) as source:
                expected = hashlib.file_digest(source, "sha256").hexdigest()
            path = paths[member.name]
            if sha256(path) != expected:
                raise ValueError(f"staged backend differs from its verified upstream archive: {member.name}")
            if os.name != "nt" and member.mode & 0o111 and not path.stat().st_mode & 0o111:
                raise ValueError(f"staged backend helper lost its executable mode: {member.name}")
    return paths


def stage(lock, target, archive_path, source_path, destination):
    verify_archive(archive_path, lock["packages"][target])
    verify_archive(source_path, lock["source"])
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tarfile.open(archive_path, "r:gz") as archive:
        members = package_members(archive, lock, target)
        with tempfile.TemporaryDirectory(prefix=".codex-stage-", dir=destination.parent) as scratch:
            staging = Path(scratch) / "codex"
            staging.mkdir()
            for member in members:
                output = staging / member.name
                if member.isdir():
                    output.mkdir(parents=True, exist_ok=True)
                    continue
                output.parent.mkdir(parents=True, exist_ok=True)
                with archive.extractfile(member) as source, output.open("xb") as sink:
                    shutil.copyfileobj(source, sink, length=1024 * 1024)
                output.chmod(0o755 if member.mode & 0o111 else 0o644)
            shutil.copyfile(source_path, staging / "SOURCE.tar.gz")
            files = bundle_files(staging, lock, target)
            if destination.exists() or destination.is_symlink():
                if destination.is_symlink() or not destination.is_dir():
                    raise ValueError("backend destination must be a real directory")
                existing = bundle_files(destination, lock, target)
                if any(sha256(existing[name]) != sha256(path) for name, path in files.items()):
                    raise ValueError("existing backend directory differs from the pinned package; choose an empty destination")
            else:
                staging.rename(destination)
    print(f"Staged Codex {lock['version']} for {target} at {destination}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", choices=TARGETS, required=True)
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--lock", type=Path, default=Path("backend.lock.json"))
    parser.add_argument("--cache", type=Path, default=Path("target/backend-downloads"))
    parser.add_argument("--archive", type=Path, help="Verify and use an existing upstream package archive")
    parser.add_argument("--source-archive", type=Path, help="Verify and use an existing upstream source archive")
    args = parser.parse_args()
    lock = read_lock(args.lock)
    package = lock["packages"][args.target]
    archive = args.archive or download(package, args.cache / package["url"].rsplit("/", 1)[1])
    source = args.source_archive or download(lock["source"], args.cache / f"codex-source-{lock['commit']}.tar.gz")
    stage(lock, args.target, archive, source, args.destination)


if __name__ == "__main__":
    main()
