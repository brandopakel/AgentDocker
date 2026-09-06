#!/bin/sh
# Install AgentDocker: the `agentdocker` CLI and the `agentd` daemon.
#
#   curl -fsSL https://raw.githubusercontent.com/brandopakel/AgentDocker/main/install.sh | sh
#
# Environment:
#   AGENTDOCKER_VERSION      a tag such as v0.2.0 (default: latest release)
#   AGENTDOCKER_INSTALL_DIR  where the binaries go (default: ~/.local/bin)
set -eu

repo="brandopakel/AgentDocker"
version="${AGENTDOCKER_VERSION:-latest}"
dir="${AGENTDOCKER_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)" in
    Darwin) os="apple-darwin" ;;
    Linux) os="unknown-linux-musl" ;;
    *) echo "install.sh: unsupported OS $(uname -s); build from source with cargo install agentdocker" >&2; exit 1 ;;
esac
case "$(uname -m)" in
    arm64 | aarch64) arch="aarch64" ;;
    x86_64 | amd64) arch="x86_64" ;;
    *) echo "install.sh: unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
target="$arch-$os"
archive="agentdocker-$target.tar.gz"
if [ "$version" = "latest" ]; then
    url="https://github.com/$repo/releases/latest/download/$archive"
else
    url="https://github.com/$repo/releases/download/$version/$archive"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "downloading $url"
curl -fsSL "$url" -o "$tmp/$archive"
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
tar -xzf "$tmp/$archive" -C "$tmp"

mkdir -p "$dir"
for bin in agentdocker agentd; do
    src="$(find "$tmp" -type f -name "$bin" | head -n 1)"
    [ -n "$src" ] || { echo "install.sh: $bin missing from $archive" >&2; exit 1; }
    install -m 0755 "$src" "$dir/$bin"
done
installed="agentdocker and agentd"
# The desktop app ships in the macOS archives only; elsewhere it is built
# from source with `cargo install --path crates/ui --locked`.
ui="$(find "$tmp" -type f -name agentdocker-ui | head -n 1)"
if [ -n "$ui" ]; then
    install -m 0755 "$ui" "$dir/agentdocker-ui"
    installed="$installed, agentdocker-ui"
fi
echo "installed $installed into $dir"

case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "add $dir to your PATH, e.g.  export PATH=\"$dir:\$PATH\"" ;;
esac
echo "the daemon starts on demand; to run it as a login service:  agentdocker daemon install"
