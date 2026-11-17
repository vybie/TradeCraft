#!/usr/bin/env python3
"""Install the Linux desktop identity and icon for this checkout."""
import os
from pathlib import Path
import shutil
import subprocess

root = Path(__file__).resolve().parent.parent
xdg = Path(os.environ.get("XDG_DATA_HOME", ""))
data = xdg if xdg.is_absolute() else Path.home() / ".local/share"
binary = root / "target/release/tradecraft"
if not binary.is_file():
    raise SystemExit("Build TradeCraft first with cargo build --release --locked")
icon = data / "icons/hicolor/256x256/apps/tradecraft.png"
desktop = data / "applications/tradecraft.desktop"
icon.parent.mkdir(parents=True, exist_ok=True)
desktop.parent.mkdir(parents=True, exist_ok=True)
shutil.copyfile(root / "apps/tradecraft/assets/tradecraft.png", icon)

def exec_quote(value):
    value = str(value).replace("%", "%%")
    for char in ('\\', '"', '`', '$'):
        value = value.replace(char, '\\' + char)
    return '"' + value + '"'

desktop.write_text(
    "[Desktop Entry]\nType=Application\nName=TradeCraft\n"
    "Comment=TradeCraft charting workspace\n"
    f"Exec={exec_quote(binary)}\nIcon={icon}\n"
    "Terminal=false\nCategories=Office;Finance;\nStartupWMClass=tradecraft\n",
    encoding="utf-8",
)
if shutil.which("update-desktop-database"):
    subprocess.run(["update-desktop-database", str(desktop.parent)], check=False)
print(f"Installed {desktop} and {icon}")
