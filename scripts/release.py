"""Package and safely publish the native release matrix using only Python's stdlib."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib

import archives
import backend

TARGETS = backend.TARGETS


def validate(tag, root=Path(".")):
    if not re.fullmatch(
        r"v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
        r"(?:-(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)"
        r"(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*)?",
        tag,
    ):
        raise ValueError("release tag must be vMAJOR.MINOR.PATCH with an optional SemVer prerelease")
    package = tomllib.loads((root / "Cargo.toml").read_text())["package"]
    lock = tomllib.loads((root / "Cargo.lock").read_text())["package"]
    versions = [entry["version"] for entry in lock if entry["name"] == package["name"]]
    if tag != f"v{package['version']}" or versions != [package["version"]]:
        raise ValueError("release tag, Cargo.toml, and Cargo.lock package versions must match")
    return package


def binary_name(tag, target):
    extension = ".exe" if target.endswith("windows-msvc") else ""
    return f"codex-acp-v2-{tag}-{target}{extension}"


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def source_archive_name():
    return f"codex-backend-sources-{backend.read_lock()['version']}.zip"


def prepare_payload(tag, target, backend_archive=None):
    package = validate(tag)
    lock = backend.read_lock()
    backend.verify_notices(lock)
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    expected_commit = os.environ.get("GITHUB_SHA")
    if expected_commit and commit != expected_commit:
        raise ValueError("checkout is not the triggering release commit")
    info = {
        "version": package["version"],
        "target": target,
        "commit": commit,
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        "backend": {
            "repository": lock["repository"], "version": lock["version"],
            "tag": lock["tag"], "commit": lock["commit"],
            "package": lock["packages"][target], "source": lock["source"],
            "correspondingSources": source_archive_name(),
            "build": "Unmodified upstream executables; this workflow assembles the distribution.",
        },
    }
    files = {
        "BUILD-INFO.json": ((json.dumps(info, indent=2) + "\n").encode(), 0o644),
    }
    upstream = backend_archive or Path("target/backend-downloads") / lock["packages"][target]["url"].rsplit("/", 1)[1]
    bundle = Path("target") / target / "release/codex"
    for name, path in backend.verified_files(bundle, upstream, lock, target).items():
        files[f"codex/{name}"] = (path, 0o644 if name in ("codex-package.json", "SOURCE.tar.gz") else 0o755)
    for name in (
        "README.md", "LICENSE", "THIRD-PARTY-NOTICES.html", "CONTRIBUTING.md",
        "SECURITY.md", "CHANGELOG.md", "AGENTS.md", "backend.lock.json",
    ):
        path = Path(name)
        if not path.is_file() or not path.stat().st_size:
            raise ValueError(f"required release file is empty: {name}")
        files[name] = (path, 0o644)
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    rust_notices = sysroot / "share" / "doc" / "rust"
    files["RUST-STD-NOTICES.html"] = (rust_notices / "COPYRIGHT-library.html", 0o644)
    for prefix, source in (("RUST-STD-LICENSES", rust_notices / "licenses"), ("licenses", Path("licenses"))):
        sources = sorted(path for path in source.rglob("*") if path.is_file())
        if not sources:
            raise ValueError(f"required license directory is missing or empty: {source}")
        for path in sources:
            if path.is_symlink():
                raise ValueError(f"release license cannot be a symlink: {path}")
            files[f"{prefix}/{path.relative_to(source).as_posix()}"] = (path, 0o644)
    entries = []
    for name, (content, mode) in sorted(files.items()):
        entries.append({"path": name,
                        "size": content.stat().st_size if isinstance(content, Path) else len(content),
                        "sha256": sha256(content) if isinstance(content, Path) else hashlib.sha256(content).hexdigest(),
                        "executable": bool(mode & 0o111)})
    manifest = (json.dumps({"schemaVersion": 1, "files": entries}, indent=2) + "\n").encode()
    if len(entries) >= 4096 or len(manifest) > 1024 * 1024:
        raise ValueError("embedded payload exceeds runtime manifest bounds")
    if any(entry["size"] > 512 * 1024 * 1024 for entry in entries) or sum(entry["size"] for entry in entries) > 1024 * 1024 * 1024:
        raise ValueError("embedded payload exceeds runtime expanded size bounds")
    files["BUNDLE-MANIFEST.json"] = (manifest, 0o644)
    payload = Path("target/packaging") / target / "backend.zip"
    archives.write_archive(payload, "", files)
    return payload


def build_release(tag, target, backend_archive=None, check=False):
    payload = prepare_payload(tag, target, backend_archive)
    environment = {**os.environ, "CODEX_ACP_BUNDLE_PATH": str(payload.resolve()),
                   "CODEX_ACP_BUNDLE_SHA256": sha256(payload)}
    if check:
        subprocess.run(["cargo", "clippy", "--locked", "--all-targets", "--features", "bundled-backend",
                        "--target", target, "--", "-D", "warnings"], env=environment,
                       check=True, timeout=900)
    subprocess.run(["cargo", "build", "--locked", "--release", "--features", "bundled-backend",
                    "--target", target], env=environment, check=True, timeout=1200)
    executable = "codex-acp-v2.exe" if target.endswith("windows-msvc") else "codex-acp-v2"
    binary = Path("target") / target / "release" / executable
    actual = subprocess.check_output([str(binary.resolve()), "--version"], text=True).strip()
    if actual != f"codex-acp-v2 {tag.removeprefix('v')}":
        raise ValueError(f"unexpected binary version: {actual}")
    subprocess.run([str(binary.resolve()), "--help"], check=True, stdout=subprocess.DEVNULL)
    destination = Path("dist") / binary_name(tag, target)
    destination.parent.mkdir(exist_ok=True)
    shutil.copyfile(binary, destination)
    destination.chmod(0o755)
    print(destination)


def verify_package(tag, target, deep):
    validate(tag)
    artifact = Path("dist") / binary_name(tag, target)
    with tempfile.TemporaryDirectory(prefix="codex-package-smoke-") as temporary:
        root = Path(temporary)
        binary = root / artifact.name
        shutil.copyfile(artifact, binary)
        binary.chmod(0o755)
        environment = {**os.environ, "CODEX_ACP_CACHE_DIR": str(root / "private-cache")}
        environment.pop("CODEX_PATH", None)
        environment.pop("CODEX_APP_SERVER_PATH", None)
        for option in ("--help", "--version"):
            subprocess.run([str(binary), option], env=environment, check=True, timeout=30,
                           stdout=subprocess.DEVNULL)
        if (root / "private-cache").exists():
            raise ValueError("help/version unexpectedly extracted the runtime")
        fixtures = ["bundled_smoke.py"]
        if deep:
            if target != "x86_64-unknown-linux-musl":
                raise ValueError("the deep packaged workflow is enabled only on Linux x86_64")
            fixtures.append("installed_workflow.py")
        for fixture in fixtures:
            subprocess.run([sys.executable, str(Path("tests/fixtures") / fixture), str(binary)],
                           check=True, timeout=240, env=environment)
        extracted = subprocess.check_output([str(binary), "--extract-runtime"], env=environment,
                                            text=True, timeout=120).strip()
        cache = Path(extracted)
        if not cache.parent.samefile(root / "private-cache") or not re.fullmatch(r"[0-9a-f]{64}", cache.name):
            raise ValueError("self-extraction escaped its isolated cache")
        manifest = json.loads((cache / "BUNDLE-MANIFEST.json").read_text(encoding="utf-8"))
        actual = {path.relative_to(cache).as_posix() for path in cache.rglob("*") if path.is_file()}
        if actual != {entry["path"] for entry in manifest["files"]} | {"BUNDLE-MANIFEST.json"}:
            raise ValueError("self-extracted payload has missing or unexpected files")
        for entry in manifest["files"]:
            backend.verify_archive(cache / entry["path"], entry)
        backend.verify_notices(backend.read_lock(), cache / "licenses/CODEX")
    print(f"Verified standalone {target} executable with its self-extracted default backend.")


def release_manifest(tag, directory=Path("dist")):
    expected = {binary_name(tag, target) for target in TARGETS} | {source_archive_name()}
    actual = {path.name for path in directory.iterdir() if path.name != "SHA256SUMS"}
    if actual != expected:
        raise ValueError(f"incomplete or unexpected release artifacts: missing={expected - actual}, extra={actual - expected}")
    return "".join(f"{sha256(directory / name)}  {name}\n" for name in sorted(expected))


def github(*arguments):
    result = subprocess.run(["gh", *arguments], text=True, capture_output=True, check=False)
    if result.returncode:
        raise RuntimeError(f"GitHub CLI failed ({result.returncode}): {result.stderr.strip()}")
    return result


def published_assets(release, paths):
    """Reject collisions rather than silently replacing an already uploaded artifact."""
    assets = {asset["name"]: asset for asset in release["assets"]}
    unexpected = assets.keys() - paths.keys()
    if unexpected:
        raise ValueError(f"release contains unexpected assets: {sorted(unexpected)}")
    for name, asset in assets.items():
        path = paths[name]
        if asset.get("digest") != f"sha256:{sha256(path)}" or asset["size"] != path.stat().st_size:
            raise ValueError(f"refusing to overwrite release asset with different or unverifiable bytes: {name}")
    return paths.keys() - assets.keys()


def publish(tag, repository):
    validate(tag)
    remote_commit = json.loads(github("api", f"repos/{repository}/commits/{tag}").stdout)["sha"]
    checkout_commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    if remote_commit != checkout_commit:
        raise ValueError("release tag moved since this workflow checked out its source")
    directory = Path("dist")
    if (directory / "SHA256SUMS").read_text() != release_manifest(tag, directory):
        raise ValueError("release checksum manifest does not match the binaries and sources")
    # The by-tag REST endpoint only returns published releases. The authenticated
    # listing includes drafts, which must be reused after an interrupted upload.
    endpoint = f"repos/{repository}/releases"
    pages = json.loads(github("api", f"{endpoint}?per_page=100", "--paginate", "--slurp").stdout)
    matches = [entry for page in pages for entry in page if entry["tag_name"] == tag]
    if len(matches) > 1:
        raise ValueError("multiple releases use this tag; refusing an ambiguous publication")
    if matches:
        release = matches[0]
    else:
        release = json.loads(github(
            "api", endpoint, "--method", "POST",
            "--raw-field", f"tag_name={tag}", "--raw-field", f"name={tag}",
            "--raw-field", f"target_commitish={checkout_commit}",
            "--field", "draft=true", "--field", "generate_release_notes=true",
            "--field", f"prerelease={str('-' in tag).lower()}",
        ).stdout)
    if release["tag_name"] != tag:
        raise ValueError("GitHub returned an unexpected release tag")
    endpoint = f"{endpoint}/{release['id']}"
    paths = {path.name: path for path in directory.iterdir()}
    missing = published_assets(release, paths)
    if not release["draft"]:
        if missing:
            raise ValueError("published release is incomplete; refusing to mutate it")
        print(f"Release {tag} is already published with identical assets.")
        return
    if missing:
        github("release", "upload", tag, "--repo", repository, *(str(paths[name]) for name in sorted(missing)))
    uploaded = json.loads(github("api", endpoint).stdout)
    if published_assets(uploaded, paths):
        raise ValueError("GitHub did not retain all verified release assets")
    github("api", endpoint, "--method", "PATCH", "--field", "draft=false")
    print(f"Published {tag} with {len(paths)} verified assets.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)
    for command in ("validate", "prepare", "build", "verify", "manifest", "publish"):
        command_parser = subcommands.add_parser(command)
        command_parser.add_argument("--tag", required=True)
        if command in ("prepare", "build", "verify"):
            command_parser.add_argument("--target", choices=TARGETS, required=True)
        if command in ("prepare", "build"):
            command_parser.add_argument("--backend-archive", type=Path)
        if command == "build":
            command_parser.add_argument("--check", action="store_true", help="Also Clippy-check the embedded production feature")
        if command == "verify":
            command_parser.add_argument("--deep", action="store_true")
        if command == "publish":
            command_parser.add_argument("--repository", required=True)
    args = parser.parse_args()
    if args.command == "validate":
        validate(args.tag)
    elif args.command == "prepare":
        print(prepare_payload(args.tag, args.target, args.backend_archive))
    elif args.command == "build":
        build_release(args.tag, args.target, args.backend_archive, args.check)
    elif args.command == "verify":
        verify_package(args.tag, args.target, args.deep)
    elif args.command == "manifest":
        validate(args.tag)
        Path("dist/SHA256SUMS").write_text(release_manifest(args.tag))
    elif args.command == "publish":
        publish(args.tag, args.repository)


if __name__ == "__main__":
    main()
