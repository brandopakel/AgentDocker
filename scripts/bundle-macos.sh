#!/bin/sh
# Wrap the desktop binary in a macOS application bundle.
#
# A bare Mach-O executable is named after its file everywhere macOS
# shows it: the Dock tile, the app switcher, the menu bar all read
# "agentdocker-ui", and it gets the generic executable icon. The name
# and the icon live in a bundle, not in the program, so this builds one.
#
#   scripts/bundle-macos.sh <binary> <output-dir> [version]
#
# leaves <output-dir>/AgentDocker.app.
set -eu

binary=${1:?usage: bundle-macos.sh <binary> <output-dir> [version]}
outdir=${2:?usage: bundle-macos.sh <binary> <output-dir> [version]}
version=${3:-0.1.0}
# The CLI and daemon travel with the app: its Runtimes and Installation
# screens shell out to `agentdocker`, and an app installed on its own
# would find whatever happens to be on PATH, or nothing.
bindir=$(dirname -- "$binary")
for tool in agentdocker agentd; do
    if [ ! -x "$bindir/$tool" ]; then
        echo "bundle-macos.sh: required executable $tool is not beside the UI binary" >&2
        exit 1
    fi
done

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
app="$outdir/AgentDocker.app"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

python3 "$root/scripts/icon.py" "$work" >/dev/null
iconutil --convert icns --output "$work/AgentDocker.icns" "$work/AgentDocker.iconset"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
# The executable keeps its own name and the *bundle* carries the
# product's, through CFBundleName below. Naming the executable
# `AgentDocker` seemed tidier and was a trap: macOS volumes are
# case-insensitive by default, so `Contents/MacOS/agentdocker` then
# resolved to the GUI, and every window button that shells out to the
# CLI ran the GUI with a CLI argument instead.
cp "$binary" "$app/Contents/MacOS/agentdocker-ui"
chmod 0755 "$app/Contents/MacOS/agentdocker-ui"
for tool in agentdocker agentd; do
    if [ -f "$bindir/$tool" ]; then
        cp "$bindir/$tool" "$app/Contents/MacOS/$tool"
        chmod 0755 "$app/Contents/MacOS/$tool"
    else
        echo "bundle-macos.sh: required executable $tool disappeared during packaging" >&2
        exit 1
    fi
done
cp "$work/AgentDocker.icns" "$app/Contents/Resources/AgentDocker.icns"

# The four-character type and creator codes. Classic Mac OS metadata
# that modern macOS mostly ignores, present in every shipping bundle
# and free to be right about.
printf 'APPL????' > "$app/Contents/PkgInfo"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key><string>AgentDocker</string>
	<key>CFBundleDisplayName</key><string>AgentDocker</string>
	<key>CFBundleExecutable</key><string>agentdocker-ui</string>
	<key>CFBundleIdentifier</key><string>dev.agentdocker.desktop</string>
	<key>CFBundleIconFile</key><string>AgentDocker</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleShortVersionString</key><string>$version</string>
	<key>CFBundleVersion</key><string>$version</string>
	<key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
	<key>LSMinimumSystemVersion</key><string>11.0</string>
	<key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
	<key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc, so the bundle runs on the machine that built it without a
# developer identity. A released build is signed and notarised on top of
# this by whoever holds the identity.
codesign --force --sign - --timestamp=none "$app" >/dev/null 2>&1 || true
echo "$app"
