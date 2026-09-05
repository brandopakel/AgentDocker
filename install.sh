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
curl -fsSL "$url.sha256" -o "$tmp/$archive.sha256" 2>/dev/null && (
    cd "$tmp" && { sha256sum -c "$archive.sha256" >/dev/null 2>&1 || shasum -a 256 -c "$archive.sha256" >/dev/null; }
) || echo "install.sh: no checksum to verify" >&2
tar -xzf "$tmp/$archive" -C "$tmp"

mkdir -p "$dir"
for bin in agentdocker agentd; do
    src="$(find "$tmp" -type f -name "$bin" | head -n 1)"
    [ -n "$src" ] || { echo "install.sh: $bin missing from $archive" >&2; exit 1; }
    install -m 0755 "$src" "$dir/$bin"
done
echo "installed agentdocker and agentd into $dir"

case ":$PATH:" in
    *":$dir:"*) ;;
    *) echo "add $dir to your PATH, e.g.  export PATH=\"$dir:\$PATH\"" ;;
esac
echo "the daemon starts on demand; to run it as a login service:  agentdocker daemon install"
