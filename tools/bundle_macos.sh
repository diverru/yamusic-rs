#!/bin/bash
# Собирает «Яндекс Музыка RS.app» для macOS: cargo build --release, Info.plist, значок .icns
# из icons/app.ico и подпись ad-hoc. Результат: target/release/bundle/Яндекс Музыка RS.app.
# Без аргументов собирает под архитектуру этого Mac, с --universal — один бинарник для
# Apple Silicon и Intel (нужны цели aarch64-apple-darwin и x86_64-apple-darwin в rustup).
# С --dmg рядом кладёт образ для установки перетаскиванием: приложение и ярлык «Программ».
set -euo pipefail
cd "$(dirname "$0")/.."

NAME="Яндекс Музыка RS"
BUNDLE_ID="io.github.spolzermod.yamusic-rs"
EXE="yamusic-rs"
VERSION=$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)
TARGET="${CARGO_TARGET_DIR:-target}"
OUT="$TARGET/release/bundle/$NAME.app"

# Без этого x86_64-срез получает минимальную версию 10.12, а Info.plist обещает 11.0.
export MACOSX_DEPLOYMENT_TARGET=11.0

UNIVERSAL=0
DMG=0
for arg in "$@"; do
  case "$arg" in
    --universal) UNIVERSAL=1 ;;
    --dmg) DMG=1 ;;
    *) echo "Использование: $0 [--universal] [--dmg]" >&2; exit 2 ;;
  esac
done

if [ "$UNIVERSAL" = 1 ]; then
  cargo build --release --target aarch64-apple-darwin
  cargo build --release --target x86_64-apple-darwin
  BIN="$TARGET/release/$EXE-universal"
  lipo -create -output "$BIN" "$TARGET/aarch64-apple-darwin/release/$EXE" "$TARGET/x86_64-apple-darwin/release/$EXE"
else
  cargo build --release
  BIN="$TARGET/release/$EXE"
fi

rm -rf "$OUT"
mkdir -p "$OUT/Contents/MacOS" "$OUT/Contents/Resources"
cp "$BIN" "$OUT/Contents/MacOS/$EXE"

# Значок: в .ico лежит 256 пикселей — из них собираются размеры до 256 (128@2x).
ICONS=$(mktemp -d)
trap 'rm -rf "$ICONS"' EXIT
mkdir "$ICONS/AppIcon.iconset"
sips -s format png icons/app.ico --out "$ICONS/app.png" >/dev/null
for spec in 16:16x16 32:16x16@2x 32:32x32 64:32x32@2x 128:128x128 256:128x128@2x 256:256x256; do
  sips -z "${spec%%:*}" "${spec%%:*}" "$ICONS/app.png" --out "$ICONS/AppIcon.iconset/icon_${spec#*:}.png" >/dev/null
done
iconutil -c icns "$ICONS/AppIcon.iconset" -o "$OUT/Contents/Resources/AppIcon.icns"

cat > "$OUT/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>
  <string>$NAME</string>
  <key>CFBundleDisplayName</key>
  <string>$NAME</string>
  <key>CFBundleIdentifier</key>
  <string>$BUNDLE_ID</string>
  <key>CFBundleExecutable</key>
  <string>$EXE</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleVersion</key>
  <string>$VERSION</string>
  <key>CFBundleShortVersionString</key>
  <string>$VERSION</string>
  <key>LSMinimumSystemVersion</key>
  <string>11.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
  <key>NSPrincipalClass</key>
  <string>NSApplication</string>
</dict>
</plist>
PLIST

codesign -s - --force "$OUT"
echo "$OUT"

if [ "$DMG" = 1 ]; then
  if [ "$UNIVERSAL" = 1 ]; then ARCH=universal; else ARCH=$(uname -m); fi
  IMAGE="$TARGET/release/bundle/yamusic-rs-macos-$ARCH.dmg"
  # Приложение и симлинк на «Программы»: в окне образа его достаточно перетащить.
  STAGE=$(mktemp -d)
  trap 'rm -rf "$ICONS" "$STAGE"' EXIT
  cp -R "$OUT" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  rm -f "$IMAGE"
  # На раннерах CI hdiutil иногда отвечает «Resource busy»: пробуем ещё.
  for attempt in 1 2 3; do
    hdiutil create -volname "$NAME" -srcfolder "$STAGE" -fs HFS+ -format UDZO -ov -quiet "$IMAGE" && break
    [ "$attempt" = 3 ] && exit 1
    sleep 3
  done
  echo "$IMAGE"
fi
