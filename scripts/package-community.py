"""Assemble the Windows community preview and its available license notices."""

import hashlib
import json
import shutil
import subprocess
import sys
from pathlib import Path
from zipfile import ZIP_DEFLATED, ZipFile

root = Path(__file__).resolve().parent.parent
metadata = json.loads(subprocess.check_output(["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"], cwd=root))
version = next(p["version"] for p in metadata["packages"] if p["name"] == "tradecraft")
out = root / "target" / "packages" / f"TradeCraft-{version}-windows-x64"
out.mkdir(parents=True, exist_ok=True)
for name in ("README.md", "ARCHITECTURE.md", "ATTRIBUTION.md", "CHANGELOG.md", "LICENSE", "RELEASE-CHECKLIST.md", f"RELEASE-NOTES-{version}.md"):
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

subprocess.run([sys.executable, str(root / "scripts/license-audit.py"), "--target", "x86_64-pc-windows-msvc", "--output", str(out / "licenses")], cwd=root, check=True)
archive = out.parent / f"{out.name}.zip"
with ZipFile(archive, "w", ZIP_DEFLATED, strict_timestamps=False) as z:
    for file in sorted(out.rglob("*")):
        if file.is_file():
            z.write(file, file.relative_to(out.parent))
print(f"Package: {archive}")
print(f"SHA256: {hashlib.sha256(archive.read_bytes()).hexdigest()}")
