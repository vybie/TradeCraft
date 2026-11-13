"""Assemble the Windows community preview and its available license notices."""

import hashlib
import json
import re
import shutil
import subprocess
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile

root = Path(__file__).resolve().parent.parent
out = root / "target" / "packages" / "TradeCraft-0.2.0-windows-x64"
out.mkdir(parents=True, exist_ok=True)
for name in ("README.md", "ARCHITECTURE.md", "ATTRIBUTION.md", "CHANGELOG.md", "LICENSE"):
    shutil.copy2(root / name, out / name)
shutil.copy2(root / "target/release/tradecraft.exe", out / "tradecraft.exe")
for source in ("licenses", "vendor/egui-charts"):
    dest = out / source
    if source == "licenses":
        shutil.copytree(root / source, dest, dirs_exist_ok=True)
    else:
        for name in ("LICENSE-MIT", "LICENSE-APACHE", "THIRD-PARTY-NOTICES.md", "assets/icons/LICENSE-lucide.txt", "assets/fonts/LICENSE-roboto.txt"):
            (dest / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(root / source / name, dest / name)

metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--offline", "--format-version", "1", "--filter-platform", "x86_64-pc-windows-msvc"], cwd=root))
tree = subprocess.check_output(["cargo", "tree", "--locked", "--offline", "--target", "x86_64-pc-windows-msvc", "-e", "normal", "--prefix", "none", "--format", "{p}"], cwd=root, text=True)
used = {match.groups() for line in tree.splitlines() if (match := re.match(r"^([^ ]+) v([^ ]+)", line))}
notices = out / "licenses" / "dependencies"
notices.mkdir(parents=True, exist_ok=True)
rows = ["# Windows community dependency license inventory", "", "Generated from the Windows normal dependency tree. License expressions are package metadata; see ARCHITECTURE.md for unresolved Craft allowlist findings. The copied notices do not constitute a legal audit.", ""]
missing = []
for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
    key = (package["name"], package["version"])
    if key not in used or package["source"] is None:
        continue
    folder = Path(package["manifest_path"]).parent
    files = [p for p in folder.iterdir() if p.is_file() and re.match(r"(?i)^(license|licence|copying|notice)(\.|-|$)", p.name)]
    license_file = package.get("license_file")
    if license_file and (folder / license_file).is_file() and (folder / license_file) not in files:
        files.append(folder / license_file)
    name = f"{key[0]}-{key[1]}"
    rows.append(f"- **{name}**: {package.get('license') or 'not declared'}; " + (", ".join(p.name for p in files) if files else "no license file in local package"))
    if not files:
        missing.append(name)
    for file in files:
        dest = notices / name / file.name
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(file, dest)
(notices / "INDEX.md").write_text("\n".join(rows) + "\n", encoding="utf-8")
archive = out.parent / f"{out.name}.zip"
with ZipFile(archive, "w", ZIP_DEFLATED, strict_timestamps=False) as z:
    for file in sorted(out.rglob("*")):
        if file.is_file():
            z.write(file, file.relative_to(out.parent))
print(f"Package: {archive}")
print(f"SHA256: {hashlib.sha256(archive.read_bytes()).hexdigest()}")
print(f"Dependencies without local license files ({len(missing)}): {', '.join(missing)}")
