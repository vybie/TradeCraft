#!/usr/bin/env python3
"""Check the locked application graph and assemble its third-party notices.

The baseline is a review trigger, not a legal opinion. Only update it after
reviewing added/changed packages, their source and their actual license files.
"""
import argparse
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BASELINE = ROOT / "licenses" / "dependency-baseline.json"
SUPPLEMENTS = {
    "accesskit": "accesskit", "accesskit_consumer": "accesskit",
    "accesskit_windows": "accesskit", "accesskit_winit": "accesskit",
    "accesskit_macos": "accesskit",
    "egui": "egui", "ecolor": "egui", "eframe": "egui", "egui-wgpu": "egui",
    "egui-winit": "egui", "egui_extras": "egui", "emath": "egui",
    "epaint": "egui", "epaint_default_fonts": "egui",
    "profiling": "profiling", "spirv": "spirv",
    "enum-map": "enum-map", "enum-map-derive": "enum-map",
    "clipboard-win": "clipboard-win",
}
OBJC_VERIFIED = {
    ("block2", "0.5.1"), ("objc-sys", "0.3.5"), ("objc2", "0.5.3"),
    ("objc2-app-kit", "0.2.2"), ("objc2-foundation", "0.2.2"),
    ("objc2-metal", "0.2.2"), ("objc2-quartz-core", "0.2.2"),
}


def cargo(*args):
    return subprocess.check_output(["cargo", *args], cwd=ROOT, text=True, encoding="utf-8")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--target", required=True, choices=["x86_64-pc-windows-msvc", "aarch64-apple-darwin", "x86_64-apple-darwin"])
    parser.add_argument("--personal", action="store_true")
    parser.add_argument("--output", type=Path, help="directory to receive the notice bundle")
    parser.add_argument("--allow-missing-notices", action="store_true", help="report missing notice texts without failing; does not approve distribution")
    parser.add_argument("--snapshot", action="store_true", help="write candidate baseline; review its diff before accepting")
    args = parser.parse_args()
    features = ["--features", "vibestrader-credential"] if args.personal else []
    host = next(line.split(": ", 1)[1] for line in subprocess.check_output(["rustc", "-vV"], text=True).splitlines() if line.startswith("host: "))
    meta = json.loads(cargo("metadata", "--locked", "--format-version", "1", "--filter-platform", args.target,
                            "--filter-platform", host, *features))
    tree = cargo("tree", "--locked", "--target", args.target, "-p", "tradecraft", *features,
                 "-e", "normal", "--prefix", "none", "--format", "{p}")
    used = {m.groups() for line in tree.splitlines() if (m := re.match(r"^([^ ]+) v([^ ]+)", line))}
    packages = sorted((p for p in meta["packages"] if (p["name"], p["version"]) in used), key=lambda p: (p["name"], p["version"]))
    if len(packages) != len(used):
        raise SystemExit("Ambiguous or missing package identities in Cargo graph")
    graph = {p["name"] + "@" + p["version"]: {"license": p["license"], "source": p["source"] or "workspace/path"} for p in packages}
    key = args.target + ("-personal" if args.personal else "-community")
    baseline = json.loads(BASELINE.read_text(encoding="utf-8")) if BASELINE.exists() else {}
    if args.snapshot:
        baseline[key] = graph
        BASELINE.write_text(json.dumps(baseline, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print(f"Wrote candidate {key}: {len(graph)} packages; review its diff.")
        return
    expected = baseline.get(key)
    if expected != graph:
        for name in sorted(set(graph) | set(expected or {})):
            if graph.get(name) != (expected or {}).get(name):
                print(f"Review dependency change {name}: {(expected or {}).get(name)} -> {graph.get(name)}", file=sys.stderr)
        raise SystemExit("Dependency baseline changed or missing; review before updating it.")
    missing = []
    rows = [f"# Dependency notices for {key}", "", "Normal dependency graph; license expressions are Cargo metadata. Build and dev dependencies require separate review. See upstream/README.md for supplemental provenance and limitations.", ""]
    destination = args.output
    if destination:
        destination.mkdir(parents=True, exist_ok=True)
        # Older package scripts wrote an inventory here; do not ship a stale one.
        (destination / "dependencies" / "INDEX.md").unlink(missing_ok=True)
        for name in ("LICENSE", "ATTRIBUTION.md", "ARCHITECTURE.md"):
            shutil.copy2(ROOT / name, destination / name)
        for relative in ("LICENSE-MIT", "LICENSE-APACHE", "THIRD-PARTY-NOTICES.md", "assets/icons/LICENSE-lucide.txt", "assets/fonts/LICENSE-roboto.txt"):
            output = destination / "vendor" / "egui-charts" / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / "vendor" / "egui-charts" / relative, output)
        shutil.copytree(ROOT / "licenses" / "upstream", destination / "upstream", dirs_exist_ok=True)
        shutil.copytree(ROOT / "licenses" / "epaint-default-fonts", destination / "epaint-default-fonts", dirs_exist_ok=True)
    for p in packages:
        name, version = p["name"], p["version"]
        if p["source"] is None:
            rows.append(f"- **{name} {version}**: {p['license'] or 'not declared'}; local source; see project attribution.")
            continue
        folder = Path(p["manifest_path"]).parent
        files = [f for f in folder.iterdir() if f.is_file() and re.match(r"(?i)^(license|licence|copying|notice)(\.|-|$)", f.name)]
        if p.get("license_file") and (folder / p["license_file"]).is_file():
            files.append(folder / p["license_file"])
        files = sorted(set(files))
        supplement = ("objc2" if (name, version) in OBJC_VERIFIED else SUPPLEMENTS.get(name)) if not files else None
        if supplement and not any((ROOT / "licenses" / "upstream" / supplement).glob("*")):
            supplement = None
        if not files and not supplement:
            missing.append(f"{name}@{version}")
        details = ", ".join(f.name for f in files) if files else (f"supplement: upstream/{supplement}" if supplement else "MISSING LICENSE TEXT")
        rows.append(f"- **{name} {version}**: {p['license'] or 'not declared'}; {details}")
        if destination:
            out = destination / "dependencies" / f"{name}-{version}"
            out.mkdir(parents=True, exist_ok=True)
            for f in files:
                shutil.copy2(f, out / f.name)
            # Preserve REUSE-style file-specific copyright notices, which are not
            # necessarily present in the generic MIT license text.
            notices = set()
            for f in (folder.rglob("*.rs") if name in ("enum-map", "enum-map-derive") else []):
                for line in f.read_text(encoding="utf-8", errors="replace").splitlines()[:25]:
                    if "SPDX-FileCopyrightText:" in line:
                        notices.add(line.split("SPDX-FileCopyrightText:", 1)[1].strip())
            if notices:
                (out / "SPDX-COPYRIGHTS.txt").write_text("\n".join(sorted(notices)) + "\n", encoding="utf-8")
    if destination:
        (destination / "INDEX.md").write_text("\n".join(rows) + "\n", encoding="utf-8")
    if missing:
        message = "Missing license text for: " + ", ".join(missing)
        if not args.allow_missing_notices:
            raise SystemExit(message)
        print("WARNING: " + message, file=sys.stderr)
    print(f"License graph and notice availability checked: {key}, {len(packages)} packages.")
    print("Review limitations in licenses/upstream/README.md and ARCHITECTURE.md.")


if __name__ == "__main__":
    main()
