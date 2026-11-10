#!/bin/sh
# Install sbx.
#
#   curl -fsSL https://toollyst.github.io/sandbox/install.sh | sh
#
# Environment:
#   SBX_VERSION       release tag to install (default: the latest release), e.g. v0.1.0
#   SBX_INSTALL_DIR   where sbx and sbx-shim go (default: ~/.local/bin)
#   SBX_DOWNLOAD_URL  where the release files are (default: GitHub Releases of toollyst/sandbox)
#
# What it does: downloads sbx-<os>-<arch>.tar.gz and its .sha256, checks the checksum, and copies sbx and sbx-shim into
# SBX_INSTALL_DIR. It does not install an executor (the VMM that runs sandboxes); that is the next step it prints:
# `sbx executor install wick`. Nothing is written outside SBX_INSTALL_DIR, and nothing runs as root.

set -eu

REPO=toollyst/sandbox

say() { printf '%s\n' "sbx-install: $*" >&2; }
die() { say "error: $*"; exit 1; }

case "$(uname -s)" in
    Linux) os=linux ;;
    Darwin) os=darwin ;;
    *) die "unsupported OS $(uname -s) (Linux and macOS are supported)" ;;
esac
case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) die "unsupported architecture $(uname -m)" ;;
esac
if [ "$os" = darwin ] && [ "$arch" != aarch64 ]; then
    die "macOS needs Apple Silicon (Hypervisor.framework on arm64)"
fi
# The macOS VMM uses the in-kernel GIC (hv_gic_*), which arrived in macOS 15.
if [ "$os" = darwin ]; then
    major=$(sw_vers -productVersion 2>/dev/null | cut -d. -f1)
    if [ -n "$major" ] && [ "$major" -lt 15 ]; then
        die "macOS 15 or later is needed (this is $(sw_vers -productVersion))"
    fi
fi

if [ -n "${SBX_DOWNLOAD_URL:-}" ]; then
    base=${SBX_DOWNLOAD_URL%/}
elif [ -n "${SBX_VERSION:-}" ]; then
    base="https://github.com/$REPO/releases/download/$SBX_VERSION"
else
    base="https://github.com/$REPO/releases/latest/download"
fi
dir=${SBX_INSTALL_DIR:-$HOME/.local/bin}
asset="sbx-$os-$arch.tar.gz"

command -v curl >/dev/null 2>&1 || die "curl is required"
if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    die "sha256sum or shasum is required"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $base/$asset"
curl -fsSL --retry 3 -o "$tmp/$asset" "$base/$asset" || die "download failed: $base/$asset"
curl -fsSL --retry 3 -o "$tmp/$asset.sha256" "$base/$asset.sha256" || die "download failed: $base/$asset.sha256"
want=$(cut -d' ' -f1 <"$tmp/$asset.sha256")
got=$(sha256 "$tmp/$asset")
[ "$want" = "$got" ] || die "checksum mismatch for $asset (expected $want, got $got)"

mkdir -p "$tmp/x"
tar -xzf "$tmp/$asset" -C "$tmp/x"
src="$tmp/x/sbx-$os-$arch"
if [ ! -x "$src/sbx" ] || [ ! -x "$src/sbx-shim" ]; then
    die "$asset does not contain sbx and sbx-shim"
fi
mkdir -p "$dir"
# sbx finds sbx-shim next to itself, so both go to the same directory.
for f in sbx sbx-shim; do
    cp "$src/$f" "$dir/$f.new" && chmod 755 "$dir/$f.new" && mv -f "$dir/$f.new" "$dir/$f"
done
say "installed $("$dir/sbx" --version) to $dir"

if [ "$os" = linux ]; then
    if [ ! -e /dev/kvm ]; then
        say "note: /dev/kvm is missing. sbx needs KVM (bare metal, or a VM with nested virtualization)."
    elif [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
        say "note: you cannot open /dev/kvm. Add yourself to its group: sudo usermod -aG $(stat -c %G /dev/kvm) \"\$USER\" (then log in again)"
    fi
fi
sbx=sbx
case ":$PATH:" in
    *":$dir:"*) ;;
    *)
        say "note: $dir is not on your PATH. Add it: export PATH=\"$dir:\$PATH\""
        sbx="$dir/sbx"
        ;;
esac
say "done. Next, install an executor (the VMM that runs sandboxes):"
say "    $sbx executor install wick"
say "then check with: $sbx info   and try: $sbx run --rm alpine echo hello"
