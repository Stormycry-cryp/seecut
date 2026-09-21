#!/bin/zsh
set -e
BIN=/Users/chenyunzhe/Documents/Codex_Project/SeeCut/Concat-main/src/target/release/concat
OUT=/Users/chenyunzhe/Documents/Codex_Project/SeeCut/outputs
APP="$OUT/Concat.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
cp "$BIN" "$APP/Contents/MacOS/Concat"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Concat</string>
    <key>CFBundleDisplayName</key><string>Concat</string>
    <key>CFBundleIdentifier</key><string>io.seecut.concat</string>
    <key>CFBundleVersion</key><string>0.2.2</string>
    <key>CFBundleShortVersionString</key><string>0.2.2</string>
    <key>CFBundleExecutable</key><string>Concat</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
# Ad-hoc signature so macOS will launch it without Gatekeeper complaints
codesign --force --deep --sign - "$APP" 2>/dev/null || true
ditto -c -k --keepParent "$APP" "$OUT/Concat-macos.zip"
ls -lh "$OUT/Concat.app/Contents/MacOS/" "$OUT/Concat-macos.zip"
