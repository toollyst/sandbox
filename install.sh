#!/bin/sh
# Install sbx.
#
#   curl -fsSL https://toollyst.github.io/sandbox/install.sh | sh
#
# Environment:
#   SBX_VERSION       release tag to install (default: the latest release), e.g. v0.1.0
#   SBX_INSTALL_DIR   where sbx and sbx-shim go (default: ~/.local/bin)
#   SBX_DOWNLOAD_URL  where the release files are (default: GitHub Releases of toollyst/sandbox)
#   SBX_VERIFY        auto (default), require or off: check the release's Sigstore signature with cosign.
#                     auto checks it when cosign is on PATH (and then a missing signature is an error, except for
#                     an SBX_VERSION before v0.2.0, which were unsigned); require also fails without cosign.
#                     With SBX_VERSION, the signature must come from that exact tag.
#
# What it does: downloads sbx-<os>-<arch>.tar.gz and its .sha256, checks the checksum and (see SBX_VERIFY) the
# Sigstore signature that release.yml made for it, and copies sbx and sbx-shim into
# SBX_INSTALL_DIR. It does not install an executor (the VMM that runs sandboxes); choosing one is the next step it
# prints. Nothing is written outside SBX_INSTALL_DIR, and nothing runs as root.

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

if [ -n "${SBX_VERSION:-}" ]; then
    case "$SBX_VERSION" in
        v[0-9]*) ;;
        *) die "SBX_VERSION must be a release tag like v0.2.0 (got $SBX_VERSION)" ;;
    esac
    case "$SBX_VERSION" in *[!0-9A-Za-z.+-]*) die "SBX_VERSION must be a release tag like v0.2.0 (got $SBX_VERSION)" ;; esac
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

# The signature says the archive was built by this repository's release workflow from a v* tag. The checksum above
# only says the download is complete; whoever controls the download URL controls both files.
verify=${SBX_VERIFY:-auto}
case "$verify" in auto | require | off) ;; *) die "SBX_VERIFY must be auto, require or off (got $verify)" ;; esac
# Which tag the signature must come from: SBX_VERSION exactly (so an older signed release can't be served in its
# place), or any v* tag for the latest release.
tag_re='v'
unsigned_ok=false
if [ -n "${SBX_VERSION:-}" ]; then
    tag_re="$(printf '%s' "$SBX_VERSION" | sed 's/[.]/[.]/g; s/[+]/[+]/g')\$"
    case "$SBX_VERSION" in v0.0.* | v0.1.*) unsigned_ok=true ;; esac
fi
if [ "$verify" != off ]; then
    if ! command -v cosign >/dev/null 2>&1; then
        [ "$verify" = auto ] || die "SBX_VERIFY=require needs cosign (https://docs.sigstore.dev/cosign/system_config/installation/)"
        say "note: cosign is not on PATH, so only the SHA-256 was checked. To check the signature too, install cosign, or"
        say "      run: gh attestation verify $asset -R $REPO"
    elif ! curl -fsSL --retry 3 -o "$tmp/$asset.sigstore.json" "$base/$asset.sigstore.json"; then
        # Whoever controls the download URL can delete the signature, so with cosign at hand a missing one is an
        # error. Only releases before v0.2.0, asked for by name, were published without one.
        if [ "$verify" = auto ] && $unsigned_ok; then
            say "note: $SBX_VERSION has no signature (releases before v0.2.0 are unsigned); only the SHA-256 was checked"
        else
            die "no signature for $asset at $base (set SBX_VERIFY=off to install without checking it)"
        fi
    else
        cosign verify-blob --bundle "$tmp/$asset.sigstore.json" \
            --certificate-identity-regexp "^https://github[.]com/$REPO/[.]github/workflows/release[.]yml@refs/tags/$tag_re" \
            --certificate-oidc-issuer https://token.actions.githubusercontent.com \
            "$tmp/$asset" >/dev/null 2>&1 || die "the Sigstore signature of $asset does not verify"
        say "verified the Sigstore signature (built by $REPO release.yml from a v* tag)"
    fi
fi

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
say "done. Next, pick an executor (the VMM that runs sandboxes) from your repositories and install it:"
say "    $sbx executor search"
say "    $sbx executor install NAME"
say "then check with: $sbx info   and try: $sbx run --rm alpine echo hello"
