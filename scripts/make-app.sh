#!/bin/zsh
set -euo pipefail

ROOT="${0:A:h:h}"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/src/target}"
BIN="${SEECUT_BIN:-$TARGET_DIR/release/concat}"
OUT="${SEECUT_OUT:-$ROOT/../outputs}"
APP="$OUT/SeeCut Preview.app"
ZIP="$OUT/SeeCut-Preview-macos.zip"
INSTALL_APP="/Applications/SeeCut Preview.app"
INSTALL_ENABLED="${SEECUT_INSTALL:-1}"
EXECUTABLE="seecut-preview"
VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/src/Cargo.toml" | head -1)"
STAMP="$(date +%Y%m%d-%H%M%S)-$$"

if [[ -n "${SEECUT_INSTALL_APP:-}" && "$SEECUT_INSTALL_APP" != "$INSTALL_APP" ]]; then
  print -u2 "安装路径固定为：$INSTALL_APP"
  exit 1
fi

[[ "$INSTALL_ENABLED" == 0 || "$INSTALL_ENABLED" == 1 ]] || { print -u2 "SEECUT_INSTALL 仅支持 0 或 1"; exit 1; }

for command in otool install_name_tool codesign ditto plutil; do
  command -v "$command" >/dev/null || { print -u2 "缺少打包工具：$command"; exit 1; }
done
[[ -x "$BIN" ]] || { print -u2 "未找到可执行文件：$BIN"; exit 1; }
[[ -n "$VERSION" ]] || { print -u2 "无法从 src/Cargo.toml 读取版本号"; exit 1; }

mkdir -p "$OUT"
STAGING_ROOT="$(mktemp -d "$OUT/.seecut-preview-stage.XXXXXX")"
STAGED_APP="$STAGING_ROOT/SeeCut Preview.app"
STAGED_ZIP="$STAGING_ROOT/SeeCut-Preview-macos.zip"
mkdir -p "$STAGED_APP/Contents/MacOS" "$STAGED_APP/Contents/Frameworks"
cp "$BIN" "$STAGED_APP/Contents/MacOS/$EXECUTABLE"

cat > "$STAGED_APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>SeeCut Preview</string>
    <key>CFBundleDisplayName</key><string>SeeCut Preview</string>
    <key>CFBundleIdentifier</key><string>cloud.stormycry.seecut.preview</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleExecutable</key><string>$EXECUTABLE</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
plutil -lint "$STAGED_APP/Contents/Info.plist" >/dev/null

# The Rust binary links FFmpeg and its Homebrew dependencies dynamically.
# Copy every non-system dylib transitively and rewrite references to @rpath so
# the preview keeps working when Homebrew is absent or upgrades its formulae.
typeset -a queue
typeset -A copied
typeset -A source_by_name
queue=("$STAGED_APP/Contents/MacOS/$EXECUTABLE")
if ! otool -l "$STAGED_APP/Contents/MacOS/$EXECUTABLE" | awk '
  $1 == "path" && $2 == "@executable_path/../Frameworks" { found = 1 }
  END { exit(found ? 0 : 1) }
'; then
  install_name_tool -add_rpath "@executable_path/../Frameworks" "$STAGED_APP/Contents/MacOS/$EXECUTABLE"
fi
while (( ${#queue[@]} > 0 )); do
  current="${queue[1]}"
  queue=("${queue[@]:1}")
  while IFS= read -r dependency; do
    case "$dependency" in
      /opt/homebrew/*|/usr/local/*)
        [[ -f "$dependency" ]] || { print -u2 "缺少动态库：$dependency"; exit 1; }
        name="${dependency:t}"
        bundled="$STAGED_APP/Contents/Frameworks/$name"
        if [[ -z "${copied[$dependency]-}" ]]; then
          if [[ -n "${source_by_name[$name]-}" ]]; then
            cmp -s "${source_by_name[$name]}" "$dependency" || {
              print -u2 "动态库文件名冲突：$name 来自 ${source_by_name[$name]} 与 $dependency"
              exit 1
            }
          else
            cp -L "$dependency" "$bundled"
            chmod u+w "$bundled"
            install_name_tool -id "@rpath/$name" "$bundled"
            source_by_name[$name]="$dependency"
            queue+=("$bundled")
          fi
          copied[$dependency]=1
        fi
        install_name_tool -change "$dependency" "@rpath/$name" "$current"
        ;;
    esac
  done < <(otool -L "$current" | tail -n +2 | awk '{print $1}')
done

for file in "$STAGED_APP/Contents/MacOS/$EXECUTABLE" "$STAGED_APP"/Contents/Frameworks/*.dylib(N); do
  if otool -L "$file" | tail -n +2 | awk '{print $1}' | grep -Eq '^(/opt/homebrew|/usr/local)/'; then
    print -u2 "仍有未打包的 Homebrew 动态库：$file"
    exit 1
  fi
done

for dylib in "$STAGED_APP"/Contents/Frameworks/*.dylib(N); do
  codesign --force --sign - "$dylib"
done
codesign --force --deep --sign - "$STAGED_APP"
codesign --verify --deep --strict --verbose=2 "$STAGED_APP"
ditto -c -k --keepParent "$STAGED_APP" "$STAGED_ZIP"

APP_BACKUP=""
ZIP_BACKUP=""
[[ ! -L "$APP" ]] || { print -u2 "拒绝替换软链接产物：$APP"; exit 1; }
[[ ! -L "$ZIP" ]] || { print -u2 "拒绝替换软链接产物：$ZIP"; exit 1; }
if [[ -e "$APP" ]]; then
  APP_BACKUP="$OUT/.SeeCut Preview.app.backup-$STAMP"
  mv "$APP" "$APP_BACKUP"
fi
if ! mv "$STAGED_APP" "$APP"; then
  [[ -z "$APP_BACKUP" ]] || mv "$APP_BACKUP" "$APP"
  exit 1
fi
if [[ -e "$ZIP" ]]; then
  ZIP_BACKUP="$OUT/.SeeCut-Preview-macos.zip.backup-$STAMP"
  mv "$ZIP" "$ZIP_BACKUP"
fi
if ! mv "$STAGED_ZIP" "$ZIP"; then
  [[ -z "$ZIP_BACKUP" ]] || mv "$ZIP_BACKUP" "$ZIP"
  exit 1
fi
rmdir "$STAGING_ROOT" 2>/dev/null || true

if [[ "$INSTALL_ENABLED" == 1 ]]; then
INSTALL_STAGE="/Applications/.SeeCut Preview.app.stage-$STAMP"
INSTALL_BACKUP=""
[[ ! -e "$INSTALL_STAGE" && ! -L "$INSTALL_STAGE" ]] || {
  print -u2 "安装暂存路径已存在：$INSTALL_STAGE"
  exit 1
}
ditto "$APP" "$INSTALL_STAGE"
codesign --verify --deep --strict --verbose=2 "$INSTALL_STAGE"
if [[ -e "$INSTALL_APP" || -L "$INSTALL_APP" ]]; then
  [[ ! -L "$INSTALL_APP" ]] || { print -u2 "拒绝替换软链接应用：$INSTALL_APP"; exit 1; }
  INSTALL_BACKUP="/Applications/.SeeCut Preview.app.backup-$STAMP"
  mv "$INSTALL_APP" "$INSTALL_BACKUP"
fi
if ! mv "$INSTALL_STAGE" "$INSTALL_APP"; then
  [[ -z "$INSTALL_BACKUP" ]] || mv "$INSTALL_BACKUP" "$INSTALL_APP"
  exit 1
fi
codesign --verify --deep --strict --verbose=2 "$INSTALL_APP"
fi

print "App: $APP"
[[ "$INSTALL_ENABLED" != 1 ]] || print "Installed: $INSTALL_APP"
print "Archive: $ZIP"
[[ -z "$APP_BACKUP" ]] || print "Previous app backup: $APP_BACKUP"
[[ -z "$ZIP_BACKUP" ]] || print "Previous archive backup: $ZIP_BACKUP"
[[ -z "${INSTALL_BACKUP:-}" ]] || print "Previous install backup: $INSTALL_BACKUP"
print "Bundled dylibs: ${#source_by_name[@]}"
