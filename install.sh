#!/bin/sh
# Install sbx and its default executor.
#
#   curl -fsSL https://toollyst.github.io/sandbox/install.sh | sh
#   curl -fsSL https://toollyst.github.io/sandbox/install.sh | sh -s -- --version    # show what it installs
#
# This script has its own version, separate from sbx's. Each version is published once as install-<version>.sh and
# never changed, so the same file always installs the same things; install.sh is a copy of the newest. The block
# below says which sbx release and which executor (with its adapter) this version installs. That pairing is a
# recommendation that was tested together: after installing, sbx and the executor are upgraded separately
# (`sbx upgrade`, `sbx executor install`), and nothing checks that they still match.
#
# Environment:
#   SBX_VERSION       release tag to install instead of the pinned one, e.g. v0.2.0
#   SBX_INSTALL_DIR   where sbx and sbx-shim go (default: ~/.local/bin)
#   SBX_DOWNLOAD_URL  where the release files are (default: GitHub Releases of toollyst/sandbox)
#   SBX_VERIFY        auto (default), require or off: check the release's Sigstore signature with cosign.
#                     auto checks it when cosign is on PATH (and then a missing signature is an error);
#                     require also fails without cosign.
#                     The signature must come from the tag being installed.
#   SBX_EXECUTOR      none: install no executor. Any other value: an executor you chose, so none is installed.
#
# What it does: downloads sbx-<os>-<arch>.tar.gz and its .sha256, checks the checksum and (see SBX_VERIFY) the
# Sigstore signature that release.yml made for it, and copies sbx and sbx-shim into SBX_INSTALL_DIR. Then it has the
# new sbx install the pinned executor from the official executor repository (`sbx executor install`, which checks the
# repository's signed index and the package's SHA-256), unless you already use another one. If that fails, sbx is
# still installed, and sbx installs the newest default executor the first time it needs one. Nothing runs as root.

set -eu

REPO=toollyst/sandbox

# BEGIN INSTALLER PINS: written by `cargo xtask installer` from installer.env. Edit installer.env, not this block.
installer_version=0.0.1
pin_sbx=0.3.0
pin_executor=
pin_adapter=
# END INSTALLER PINS

say() { printf '%s\n' "sbx-install: $*" >&2; }
die() { say "error: $*"; exit 1; }

summary() {
    s="installer $installer_version: sbx $pin_sbx"
    if [ -n "$pin_executor" ]; then
        s="$s, executor $pin_executor"
        [ -z "$pin_adapter" ] || s="$s (adapter $pin_adapter)"
    else
        s="$s, no executor pinned (sbx installs the default one on first use)"
    fi
    printf '%s\n' "$s"
}

case "${1:-}" in
    "") ;;
    --version | -V)
        summary
        exit 0
        ;;
    *) die "unknown argument $1 (the only one is --version)" ;;
esac
say "$(summary)"

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

tag="v$pin_sbx"
if [ -n "${SBX_VERSION:-}" ]; then
    case "$SBX_VERSION" in
        v[0-9]*) ;;
        *) die "SBX_VERSION must be a release tag like v0.2.0 (got $SBX_VERSION)" ;;
    esac
    case "$SBX_VERSION" in *[!0-9A-Za-z.+-]*) die "SBX_VERSION must be a release tag like v0.2.0 (got $SBX_VERSION)" ;; esac
    tag=$SBX_VERSION
    [ "$tag" = "v$pin_sbx" ] || say "installing sbx $tag (SBX_VERSION) instead of $pin_sbx"
fi
if [ -n "${SBX_DOWNLOAD_URL:-}" ]; then
    base=${SBX_DOWNLOAD_URL%/}
else
    base="https://github.com/$REPO/releases/download/$tag"
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

# The signature says the archive was built by this repository's release workflow from the tag being installed (so an
# older signed release can't be served in its place). The checksum above only says the download is complete; whoever
# controls the download URL controls both files.
verify=${SBX_VERIFY:-auto}
case "$verify" in auto | require | off) ;; *) die "SBX_VERIFY must be auto, require or off (got $verify)" ;; esac
tag_re="$(printf '%s' "$tag" | sed 's/[.]/[.]/g; s/[+]/[+]/g')\$"
if [ "$verify" != off ]; then
    if ! command -v cosign >/dev/null 2>&1; then
        [ "$verify" = auto ] || die "SBX_VERIFY=require needs cosign (https://docs.sigstore.dev/cosign/system_config/installation/)"
        say "note: cosign is not on PATH, so only the SHA-256 was checked. To check the signature too, install cosign, or"
        say "      run: gh attestation verify $asset -R $REPO"
    elif ! curl -fsSL --retry 3 -o "$tmp/$asset.sigstore.json" "$base/$asset.sigstore.json"; then
        # Whoever controls the download URL can delete the signature, so with cosign at hand a missing one is an
        # error. Every release is signed.
        die "no signature for $asset at $base (set SBX_VERIFY=off to install without checking it)"
    else
        cosign verify-blob --bundle "$tmp/$asset.sigstore.json" \
            --certificate-identity-regexp "^https://github[.]com/$REPO/[.]github/workflows/release[.]yml@refs/tags/$tag_re" \
            --certificate-oidc-issuer https://token.actions.githubusercontent.com \
            "$tmp/$asset" >/dev/null 2>&1 || die "the Sigstore signature of $asset does not verify"
        say "verified the Sigstore signature (built by $REPO release.yml from tag $tag)"
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

# The executor: the VMM that runs sandboxes. Any executor that speaks the open executor protocol works; this installer
# pins the default one from the official repository. It doesn't replace one you chose.
later="sbx installs the newest default executor from the official repository the first time it needs one"
executor_ok=false
if [ "${SBX_EXECUTOR:-}" = none ]; then
    say "SBX_EXECUTOR=none: no executor installed (choose one later with: sbx executor search)"
elif [ -n "${SBX_EXECUTOR:-}" ]; then
    say "SBX_EXECUTOR is set, so no executor is installed (sbx uses yours)"
elif [ -z "$pin_executor" ]; then
    say "no executor installed now: $later"
else
    # What new sandboxes use now, asked away from any .sbx-executor folder file: "NAME@VERSION (why)", or nothing.
    current=$(cd / && "$dir/sbx" executor current 2>/dev/null) || current=
    current=${current%% *}
    want_name=${pin_executor%%@*}
    if [ -n "$current" ] && [ "${current%%@*}" != "$want_name" ]; then
        say "keeping your executor $current (this installer pins $pin_executor; install it with: sbx executor install $pin_executor)"
    else
        say "installing the executor $pin_executor from the official repository"
        if [ -n "$pin_adapter" ] && ! "$dir/sbx" executor adapter install --repo official "$pin_adapter" >&2; then
            say "warning: could not install the adapter $pin_adapter (see above)"
        elif ! "$dir/sbx" executor install --repo official "$pin_executor" >&2; then
            say "warning: could not install $pin_executor (see above); $later, or run: sbx executor install $pin_executor"
        else
            executor_ok=true
            # An older version of the same executor was the default: the pinned one takes its place.
            if [ -n "$current" ] && [ "$current" != "$pin_executor" ]; then
                "$dir/sbx" executor use "$pin_executor" >/dev/null && say "now using $pin_executor (was $current)"
            fi
        fi
    fi
fi

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
say "done. Try: $sbx run --rm alpine echo hello"
$executor_ok || say "(to pick an executor yourself instead: $sbx executor search, then $sbx executor install NAME)"
