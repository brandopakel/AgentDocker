#!/bin/sh
# Install AgentDocker: the desktop app with the `agentdocker` CLI and the
# `agentd` daemon inside it, or the two commands alone.
#
#   curl -fsSL https://raw.githubusercontent.com/brandopakel/AgentDocker/main/install.sh | sh
#
# Environment:
#   AGENTDOCKER_VERSION      a tag such as v0.2.0 (default: latest release)
#   AGENTDOCKER_INSTALL      `desktop` (the app, installed by its own
#                            installer with rollback and in-app updates;
#                            the default on macOS) or `cli` (the two
#                            commands copied into AGENTDOCKER_INSTALL_DIR;
#                            the default on Linux)
#   AGENTDOCKER_INSTALL_DIR  where the `cli` route puts the binaries
#                            (default: ~/.local/bin)
set -eu

repo="brandopakel/AgentDocker"
version="${AGENTDOCKER_VERSION:-latest}"
dir="${AGENTDOCKER_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)" in
    Darwin) os="apple-darwin"; route="${AGENTDOCKER_INSTALL:-desktop}" ;;
    Linux) os="unknown-linux-musl"; route="${AGENTDOCKER_INSTALL:-cli}" ;;
    *) echo "install.sh: unsupported OS $(uname -s); build from source with cargo install agentdocker" >&2; exit 1 ;;
esac
case "$(uname -m)" in
    arm64 | aarch64) arch="aarch64" ;;
    x86_64 | amd64) arch="x86_64" ;;
    *) echo "install.sh: unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
case "$route" in
    desktop)
        # The desktop archives carry all three binaries and the native build
        # manifest the app's installer verifies. Linux desktop builds are
        # glibc; the commands-only archives are static musl.
        case "$os" in
            apple-darwin) target="$arch-apple-darwin"; archive="agentdocker-desktop-$target.zip" ;;
            *) target="$arch-unknown-linux-gnu"; archive="agentdocker-desktop-$target.tar.gz" ;;
        esac ;;
    cli) target="$arch-$os"; archive="agentdocker-$target.tar.gz" ;;
    *) echo "install.sh: AGENTDOCKER_INSTALL must be desktop or cli, not $route" >&2; exit 1 ;;
esac
if [ "$version" = "latest" ]; then
    url="https://github.com/$repo/releases/latest/download/$archive"
else
    url="https://github.com/$repo/releases/download/$version/$archive"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "downloading $url"
if ! curl -fsSL "$url" -o "$tmp/$archive"; then
    if [ "$route" = desktop ] && [ "$version" = latest ]; then
        # Releases before the desktop archives existed (v0.1.0) have only
        # the commands; say so and install those rather than failing.
        echo "install.sh: this release has no desktop archive; installing the commands instead (set AGENTDOCKER_VERSION to a release that has one)"
        route=cli; target="$arch-$os"; archive="agentdocker-$target.tar.gz"
        url="https://github.com/$repo/releases/latest/download/$archive"
        echo "downloading $url"
        curl -fsSL "$url" -o "$tmp/$archive"
    else
        echo "install.sh: download failed; nothing installed" >&2
        exit 1
    fi
fi
curl -fsSL "$url.sha256" -o "$tmp/$archive.sha256" || {
    echo "install.sh: checksum download failed; nothing installed" >&2
    exit 1
}
# Accept only one checksum for the archive being installed. Never let a checksum
# file name arbitrary local paths or silently turn a mismatch into success.
expected="$(awk 'NR == 1 { print $1 }' "$tmp/$archive.sha256")"
case "$expected" in
    *[!0-9a-fA-F]* | "") echo "install.sh: invalid checksum" >&2; exit 1 ;;
esac
[ "${#expected}" -eq 64 ] || { echo "install.sh: invalid checksum length" >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "$tmp/$archive" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')"
else
    echo "install.sh: sha256sum or shasum is required; nothing installed" >&2
    exit 1
fi
expected="$(printf '%s' "$expected" | tr 'A-F' 'a-f')"
[ "$actual" = "$expected" ] || { echo "install.sh: checksum mismatch; nothing installed" >&2; exit 1; }
case "$archive" in
    *.zip)
        if command -v ditto >/dev/null 2>&1; then
            ditto -x -k "$tmp/$archive" "$tmp"
        else
            unzip -q "$tmp/$archive" -d "$tmp"
        fi ;;
    *) tar -xzf "$tmp/$archive" -C "$tmp" ;;
esac

# The desktop route hands over to the app's own installer, which keeps
# retained versions with rollback, links the commands into ~/.local/bin,
# places the app in Applications, and is what `agentdocker desktop update`
# maintains afterwards. Nothing here is copied by hand.
if [ "$route" = desktop ]; then
    case "$os" in
        apple-darwin) payload="$tmp/AgentDocker.app"; cli="$payload/Contents/MacOS/agentdocker" ;;
        *) payload="$tmp/agentdocker-desktop"; cli="$payload/bin/agentdocker" ;;
    esac
    [ -x "$cli" ] || { echo "install.sh: $archive does not carry the desktop payload; nothing installed" >&2; exit 1; }
    # Released apps are ad-hoc signed until there is a Developer ID behind
    # them, which the installer only accepts as a local preview. The flag
    # goes the day the signature arrives.
    "$cli" desktop install --from "$payload" --local-preview
    echo "installed the desktop; \`agentdocker desktop status\` shows it, \`agentdocker desktop update\` keeps it current"
    case ":$PATH:" in
        *":$HOME/.local/bin:"*) ;;
        *) echo "add $HOME/.local/bin to your PATH, e.g.  export PATH=\"$HOME/.local/bin:\$PATH\"" ;;
    esac
    exit 0
fi

# A managed desktop installation (`agentdocker desktop install`, `make
# install`, or a downloaded update) owns its launchers through links into
# its retained versions. This installer copies files, so writing over those
# links would break that installation rather than update it. Refuse before
# touching anything, and name the command that does update it. Only the
# places this run would write are checked: the bundle link matters only
# when the archive carries a bundle.
managed="$HOME/.local/share/agentdocker/desktop"
app="$(find "$tmp" -type d -name AgentDocker.app | head -n 1)"
for path in "$dir/agentdocker" "$dir/agentd" "$dir/agentdocker-ui" ${app:+"$HOME/Applications/AgentDocker.app"}; do
    [ -L "$path" ] || continue
    case "$(readlink "$path")" in
        "$managed"/* | /Applications/AgentDocker.app)
            echo "install.sh: $path belongs to a managed desktop installation; update it with \`agentdocker desktop update\` or \`agentdocker desktop install --from <payload>\`, or remove it with \`agentdocker desktop uninstall\` first; nothing installed" >&2
            exit 1 ;;
    esac
done

mkdir -p "$dir"
for bin in agentdocker agentd; do
    src="$(find "$tmp" -type f -name "$bin" | head -n 1)"
    [ -n "$src" ] || { echo "install.sh: $bin missing from $archive" >&2; exit 1; }
    install -m 0755 "$src" "$dir/$bin"
done
installed="agentdocker and agentd"
# Current commands-only archives carry just the two commands; the v0.1.0
# Mac archives also carried the UI and the bundle, and are still installed.
ui="$(find "$tmp" -type f -name agentdocker-ui | head -n 1)"
if [ -n "$ui" ]; then
    install -m 0755 "$ui" "$dir/agentdocker-ui"
    installed="$installed, agentdocker-ui"
fi
# And the bundle, which is what macOS reads the name and the icon from.
# Into the user's own Applications: this installer never asks for a
# password, so it does not write to /Applications.
# The new bundle is staged beside the old one and swapped in with two
# renames, so a failure part-way leaves either the old bundle or the new
# one in place, never half of each; the old bundle is kept as
# AgentDocker.app.previous until the new one has been tried.
if [ -n "$app" ]; then
    apps="$HOME/Applications"
    current="$apps/AgentDocker.app"
    previous="$apps/AgentDocker.app.previous"
    staging="$apps/.AgentDocker.app.staging.$$"
    mkdir -p "$apps"
    rm -rf "$staging"
    cp -R "$app" "$staging" || { rm -rf "$staging"; echo "install.sh: could not stage AgentDocker.app; the existing bundle is untouched" >&2; exit 1; }
    if [ -e "$current" ] || [ -L "$current" ]; then
        rm -rf "$previous"
        mv "$current" "$previous"
    fi
    if mv "$staging" "$current"; then
        installed="$installed, AgentDocker.app"
        [ -e "$previous" ] && echo "previous bundle kept at $previous; remove it once the new one works"
    else
        [ -e "$previous" ] && mv "$previous" "$current"
        rm -rf "$staging"
        echo "install.sh: could not place AgentDocker.app; the previous bundle is back" >&2
        exit 1
    fi
fi
echo "installed $installed into $dir"

case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "add $dir to your PATH, e.g.  export PATH=\"$dir:\$PATH\"" ;;
esac
echo "the daemon starts on demand; to run it as a login service:  agentdocker daemon install"
