#!/bin/sh
# Builds "Code Atlas.app" from the release binary and puts it in ~/Applications (decision 0017).
# Finder and Spotlight open it like any other app; it runs `catlas` in its own window.
set -eu
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
cargo build --release
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app="$HOME/Applications/Code Atlas.app"
mkdir -p "$HOME/Applications"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS"
cp target/release/catlas "$app/Contents/MacOS/catlas"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Code Atlas</string>
  <key>CFBundleDisplayName</key><string>Code Atlas</string>
  <key>CFBundleIdentifier</key><string>studio.atelierastra.codeatlas</string>
  <key>CFBundleExecutable</key><string>catlas</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
</dict></plist>
PLIST
codesign --force --sign - "$app" >/dev/null 2>&1
echo "$app"
