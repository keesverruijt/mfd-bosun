#!/bin/sh
# One-line install of the latest mfd-bosun, on Linux (a Raspberry Pi or a PC) and macOS:
#   curl -fsSL https://github.com/keesverruijt/mfd-bosun/releases/latest/download/install.sh | sh
# Attached as install.sh to each GitHub release (see .github/workflows/release.yml); MFD_BOSUN_NIGHTLY=1
# installs the latest build of main instead. On Linux it fetches the release archive for this machine and
# runs the install.sh in it, which sets up the systemd service; on macOS it puts the binary in
# /usr/local/bin. Run it again to update.
set -eu

REPO=https://github.com/keesverruijt/mfd-bosun/releases
if [ -n "${MFD_BOSUN_NIGHTLY:-}" ]; then
  URL=${MFD_BOSUN_URL:-$REPO/download/nightly}
else
  URL=${MFD_BOSUN_URL:-$REPO/latest/download}
fi

fail() {
  echo "mfd-bosun: $*" >&2
  exit 1
}

# sudo only when needed: root on a Pi, or a writable /usr/local/bin on a Mac, can do without
as_root() {
  if [ "$(id -u)" -eq 0 ]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  else
    fail "needs root for: $*"
  fi
}

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO "$2" "$1"; }
else
  fail "needs curl or wget"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

case "$(uname -s)" in
  Linux)
    # The binaries are static, so a 64-bit kernel runs the aarch64 one even under a 32-bit userland
    case "$(uname -m)" in
      aarch64 | arm64) target=aarch64-unknown-linux-musl ;;
      armv7* | armv8*) target=armv7-unknown-linux-musleabihf ;;
      x86_64 | amd64) target=x86_64-unknown-linux-musl ;;
      *) fail "no build for $(uname -m) (a Raspberry Pi Zero or 1 is too old); see README.md to build from source" ;;
    esac
    echo "Downloading mfd-bosun for $target"
    fetch "$URL/mfd-bosun-$target.tar.gz" "$tmp/mfd-bosun.tar.gz"
    tar xzf "$tmp/mfd-bosun.tar.gz" -C "$tmp"
    as_root "$tmp/mfd-bosun-$target/install.sh"
    ;;
  Darwin)
    echo "Downloading mfd-bosun for macOS"
    fetch "$URL/mfd-bosun-macos" "$tmp/mfd-bosun"
    chmod 755 "$tmp/mfd-bosun"
    if [ -w /usr/local/bin ]; then
      mv "$tmp/mfd-bosun" /usr/local/bin/mfd-bosun
    else
      as_root mkdir -p /usr/local/bin
      as_root mv "$tmp/mfd-bosun" /usr/local/bin/mfd-bosun
    fi
    echo "mfd-bosun is installed in /usr/local/bin: run mfd-bosun to start it"
    ;;
  *)
    fail "no build for $(uname -s); on Windows use install.ps1, see README.md"
    ;;
esac
