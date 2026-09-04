"""Package and safely publish the native release matrix using only Python's stdlib."""

import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tomllib
import zipfile


TARGETS = (
    "aarch64-apple-darwin",
    "aarch64-unknown-linux-musl",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-musl",
)


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


def archive_name(tag, target):
    extension = "zip" if target.endswith("windows-msvc") else "tar.gz"
    return f"codex-acp-v2-{tag}-{target}.{extension}"


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def package_release(tag, target):
    package = validate(tag)
    executable = "codex-acp-v2.exe" if target.endswith("windows-msvc") else "codex-acp-v2"
    binary = Path("target") / target / "release" / executable
    actual = subprocess.check_output([str(binary.resolve()), "--version"], text=True).strip()
    if actual != f"codex-acp-v2 {package['version']}":
        raise ValueError(f"unexpected binary version: {actual}")
    subprocess.run([str(binary.resolve()), "--help"], check=True, stdout=subprocess.DEVNULL)
    commit = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    expected_commit = os.environ.get("GITHUB_SHA")
    if expected_commit and commit != expected_commit:
        raise ValueError("checkout is not the triggering release commit")
    info = {
        "version": package["version"],
        "target": target,
        "commit": commit,
        "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
    }
    files = {
        executable: binary.read_bytes(),
        "BUILD-INFO.json": (json.dumps(info, indent=2) + "\n").encode(),
    }
    for name in (
        "README.md", "LICENSE", "THIRD-PARTY-NOTICES.html", "CONTRIBUTING.md",
        "SECURITY.md", "CHANGELOG.md", "AGENTS.md",
    ):
        files[name] = Path(name).read_bytes()
        if not files[name]:
            raise ValueError(f"required release file is empty: {name}")
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip())
    rust_notices = sysroot / "share" / "doc" / "rust"
    files["RUST-STD-NOTICES.html"] = (rust_notices / "COPYRIGHT-library.html").read_bytes()
    for prefix, source in (("RUST-STD-LICENSES", rust_notices / "licenses"), ("licenses", Path("licenses"))):
        sources = sorted(path for path in source.rglob("*") if path.is_file())
        if not sources:
            raise ValueError(f"required license directory is missing or empty: {source}")
        for path in sources:
            files[f"{prefix}/{path.relative_to(source).as_posix()}"] = path.read_bytes()
    destination = Path("dist")
    destination.mkdir(exist_ok=True)
    archive = destination / archive_name(tag, target)
    directory = archive.name.removesuffix(".tar.gz").removesuffix(".zip")
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as output:
            for name, content in sorted(files.items()):
                entry = zipfile.ZipInfo(f"{directory}/{name}")
                entry.create_system = 3
                entry.external_attr = (0o100755 if name == executable else 0o100644) << 16
                entry.compress_type = zipfile.ZIP_DEFLATED
                output.writestr(entry, content)
    else:
        with archive.open("wb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode="w") as output:
                    for name, content in sorted(files.items()):
                        entry = tarfile.TarInfo(f"{directory}/{name}")
                        entry.size = len(content)
                        entry.mode = 0o755 if name == executable else 0o644
                        output.addfile(entry, io.BytesIO(content))
    print(archive)


def release_manifest(tag, directory=Path("dist")):
    expected = {archive_name(tag, target) for target in TARGETS}
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
        raise ValueError("release checksum manifest does not match the archives")
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
    for command in ("validate", "package", "manifest", "publish"):
        command_parser = subcommands.add_parser(command)
        command_parser.add_argument("--tag", required=True)
        if command == "package":
            command_parser.add_argument("--target", choices=TARGETS, required=True)
        if command == "publish":
            command_parser.add_argument("--repository", required=True)
    args = parser.parse_args()
    if args.command == "validate":
        validate(args.tag)
    elif args.command == "package":
        package_release(args.tag, args.target)
    elif args.command == "manifest":
        validate(args.tag)
        Path("dist/SHA256SUMS").write_text(release_manifest(args.tag))
    elif args.command == "publish":
        publish(args.tag, args.repository)


if __name__ == "__main__":
    main()
