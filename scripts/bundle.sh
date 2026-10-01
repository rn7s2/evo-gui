#!/usr/bin/env bash
#
# Build evo-desktop and assemble the launchable bundle: dist/Evo Desktop.app.
#
#   scripts/bundle.sh              build (release) and bundle
#   scripts/bundle.sh --no-build   bundle the release binary that is already built
#
# The build goes through cargo, so it takes the shared target dir's lock and
# waits for any other gpui build rather than racing it.
set -euo pipefail

case "${1:-}" in
--no-build) build=no ;;
"") build=yes ;;
*)
    echo "usage: ${0##*/} [--no-build]" >&2
    exit 2
    ;;
esac

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="$ROOT/dist/Evo Desktop.app"
BINARY="${CARGO_TARGET_DIR:-$ROOT/target}/release/evo-desktop"
ICON="$ROOT/assets/icon/AppIcon.icns"

VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -n 1)"
if [ -z "$VERSION" ]; then
    echo "bundle: cannot read the workspace version from Cargo.toml" >&2
    exit 1
fi
YEAR="$(date -u +%Y)"

if [ "$build" = yes ]; then
    (cd "$ROOT" && cargo build --release -p evo-desktop)
fi

if [ ! -x "$BINARY" ]; then
    echo "bundle: no executable at $BINARY" >&2
    exit 1
fi

if [ ! -f "$ICON" ]; then
    echo "bundle: no app icon at $ICON" >&2
    echo "bundle: rebuild it with: python3 assets/icon/make_icon.py" >&2
    exit 1
fi

# Rebuilt from scratch, so nothing stale survives a re-bundle.
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BINARY" "$APP/Contents/MacOS/evo-desktop"
chmod 755 "$APP/Contents/MacOS/evo-desktop"
cp "$ICON" "$APP/Contents/Resources/AppIcon.icns"

cat >"$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>evo-desktop</string>
	<key>CFBundleDisplayName</key>
	<string>Evo Desktop</string>
	<key>CFBundleIconFile</key>
	<string>AppIcon</string>
	<key>CFBundleIdentifier</key>
	<string>com.evo.desktop</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>Evo Desktop</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>$VERSION</string>
	<key>CFBundleVersion</key>
	<string>$VERSION</string>
	<key>LSApplicationCategoryType</key>
	<string>public.app-category.developer-tools</string>
	<key>LSMinimumSystemVersion</key>
	<string>15.0</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSHumanReadableCopyright</key>
	<string>Copyright © $YEAR evo-desktop contributors</string>
	<!-- The app draws its own colours and follows the system appearance. -->
	<key>NSRequiresAquaSystemAppearance</key>
	<false/>
</dict>
</plist>
PLIST

plutil -lint "$APP/Contents/Info.plist" >/dev/null

# Ad-hoc only: this bundle is built locally and never notarized.
codesign --force --sign - --identifier com.evo.desktop "$APP"
codesign --verify "$APP"

echo "bundle: $APP"
