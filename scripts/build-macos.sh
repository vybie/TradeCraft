#!/bin/bash
set -euo pipefail

cd "$(dirname "$0")/.."
if [[ $(uname -s) != Darwin ]]; then
  echo 'Run this script on macOS with Xcode command line tools and Rust installed.' >&2
  exit 1
fi

target=${1:-$(rustc -vV | sed -n 's/^host: //p')}
case "$target" in
  aarch64-apple-darwin|x86_64-apple-darwin) ;;
  *) echo "Unsupported macOS target: $target" >&2; exit 1 ;;
esac
version=$(cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "tradecraft"))')
out="target/packages/TradeCraft-${version}-macos-${target%%-*}"
app="$out/TradeCraft.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
python3 scripts/license-audit.py --target "$target" --output "$app/Contents/Resources/licenses" --allow-missing-notices
cargo build --release --locked --target "$target"
cp "target/$target/release/tradecraft" "$app/Contents/MacOS/tradecraft"
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>TradeCraft</string>
<key>CFBundleDisplayName</key><string>TradeCraft</string>
<key>CFBundleIdentifier</key><string>com.sehgalsoftware.tradecraft</string>
<key>CFBundleExecutable</key><string>tradecraft</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>$version</string>
<key>CFBundleVersion</key><string>$version</string>
<key>CFBundleIconFile</key><string>TradeCraft</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
EOF
iconset="$out/TradeCraft.iconset"
mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" apps/tradecraft/assets/tradecraft.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
  doubled=$((size * 2))
  sips -z "$doubled" "$doubled" apps/tradecraft/assets/tradecraft.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/TradeCraft.icns"
rm -r "$iconset"
cp README.md ARCHITECTURE.md ATTRIBUTION.md CHANGELOG.md LICENSE "$out/"
ditto licenses "$out/licenses"
mkdir -p "$out/vendor/egui-charts/assets/icons" "$out/vendor/egui-charts/assets/fonts"
cp vendor/egui-charts/LICENSE-MIT vendor/egui-charts/LICENSE-APACHE vendor/egui-charts/THIRD-PARTY-NOTICES.md "$out/vendor/egui-charts/"
cp vendor/egui-charts/assets/icons/LICENSE-lucide.txt "$out/vendor/egui-charts/assets/icons/"
cp vendor/egui-charts/assets/fonts/LICENSE-roboto.txt "$out/vendor/egui-charts/assets/fonts/"
ditto -c -k --keepParent "$out" "$out.zip"
echo "Unsigned package: $out.zip"
shasum -a 256 "$out.zip"
