"""Report RustSec findings in the pinned backend's shipped normal/build dependency graph."""

import json
from pathlib import Path
import re
import subprocess

import backend


def report(workspace, environment, destination):
    production = set()
    targets = {}
    for target in backend.TARGETS:
        roots = ["codex-app-server", "codex-code-mode-host"]
        if "linux" in target:
            roots.append("codex-bwrap")
        if "windows" in target:
            roots.append("codex-windows-sandbox")
        command = ["cargo", "tree", "--offline", "--locked", "--prefix", "none",
                   "--edges", "normal,build", "--format", "{p} features=[{f}]", "--target", target]
        for package in roots:
            command.extend(("--package", package))
        tree = subprocess.check_output(command, cwd=workspace, env=environment, text=True, timeout=120)
        selected = {}
        for line in tree.splitlines():
            match = re.match(r"^(\S+) v(\S+)(?: |$)", line)
            if match:
                features = re.search(r" features=\[([^]]*)\]", line)
                if features is None:
                    raise ValueError("Cargo omitted production dependency feature metadata")
                selected.setdefault(match.groups(), set()).update(filter(None, features[1].split(",")))
        if not selected:
            raise ValueError("backend production dependency graph is unexpectedly empty")
        production.update(selected)
        targets[target] = [{"name": name, "version": version, "features": sorted(features)}
                           for (name, version), features in sorted(selected.items())]
    audit = subprocess.run(["cargo-audit", "audit", "--file", str(workspace / "Cargo.lock"), "--json"],
                           text=True, capture_output=True, check=False, timeout=300)
    if audit.returncode not in (0, 1):
        raise RuntimeError(f"backend audit could not run: {audit.stderr}")
    result = json.loads(audit.stdout)

    def selected(entry):
        package = entry.get("package", {})
        return (package.get("name"), package.get("version")) in production

    findings = [entry for entry in result["vulnerabilities"]["list"] if selected(entry)]
    warnings = {kind: [entry for entry in entries if selected(entry)]
                for kind, entries in result["warnings"].items()}
    report = {
        "schemaVersion": 1, "backend": backend.read_lock(),
        "policy": "Report only for unmodified upstream binaries; strict adapter audit remains a separate required gate.",
        "scope": "Normal/build closure for shipped Rust executables across the four targets; not all workspace/dev crates. Non-Rust helper advisories require separate upstream review.",
        "productionPackageCounts": {target: len(packages) for target, packages in targets.items()},
        "productionPackages": targets, "database": result.get("database"),
        "vulnerabilities": findings, "warnings": {kind: entries for kind, entries in warnings.items() if entries},
    }
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"Bundled upstream backend: {len(findings)} RustSec vulnerability records in the shipped dependency closure (report-only).")
    for entry in findings:
        print(f"  {entry['advisory']['id']}: {entry['package']['name']} {entry['package']['version']}")
    print(f"Review the full backend report: {destination}")
