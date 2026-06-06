#!/usr/bin/env bash
# Build a release binary and wrap it in a macOS .app bundle, then zip it.
#
#   ./scripts/bundle.sh            # builds + bundles for the host arch
#
# Output: dist/Zenith.app and dist/Zenith-macos-<arch>.zip
set -euo pipefail

cd "$(dirname "$0")/.."

APP_NAME="Zenith"
BIN_NAME="zenith-launcher"
BUNDLE_ID="dev.zeriax.zenith"
VERSION="${VERSION:-$(grep -m1 '^version' Cargo.toml | sed -E 's/.*"(.*)".*/\1/')}"
ARCH="$(uname -m)" # arm64 or x86_64

echo "==> Building release ($ARCH) v$VERSION"
cargo build --release --bin "$BIN_NAME"

APP="dist/$APP_NAME.app"
echo "==> Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "target/release/$BIN_NAME" "$APP/Contents/MacOS/$BIN_NAME"

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>$APP_NAME</string>
    <key>CFBundleDisplayName</key><string>$APP_NAME</string>
    <key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleExecutable</key><string>$BIN_NAME</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>LSApplicationCategoryType</key><string>public.app-category.games</string>
</dict>
</plist>
PLIST

# Optional icon (dist/AppIcon.icns) if present
if [ -f "assets/AppIcon.icns" ]; then
    cp "assets/AppIcon.icns" "$APP/Contents/Resources/AppIcon.icns"
fi

# Ad-hoc sign so it at least launches locally without "damaged" errors
codesign --force --deep --sign - "$APP" 2>/dev/null || echo "   (codesign skipped)"

ZIP="dist/$APP_NAME-macos-$ARCH.zip"
echo "==> Zipping $ZIP"
rm -f "$ZIP"
( cd dist && ditto -c -k --keepParent "$APP_NAME.app" "$(basename "$ZIP")" )

# Drag-to-install .dmg (app + Applications symlink)
DMG="dist/$APP_NAME-macos-$ARCH.dmg"
echo "==> Building $DMG"
STAGE="dist/.dmg-stage"
rm -rf "$STAGE" "$DMG"
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
hdiutil create -volname "$APP_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG" >/dev/null
rm -rf "$STAGE"

echo "==> Done: $APP, $ZIP, $DMG"
