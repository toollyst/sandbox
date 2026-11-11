#!/usr/bin/env bash
# install.sh의 확인 분기를 가짜 릴리스(file://)와 가짜 cosign으로 돌려 본다. 네트워크도 진짜 cosign도 쓰지 않는다.
# ci.yml(test, Linux·macOS)이 돌린다. 저장소 루트에서:
#
#   .github/scripts/test-install.sh
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
case "$(uname -s)" in Linux) os=linux ;; Darwin) os=darwin ;; *) echo "skip: $(uname -s)"; exit 0 ;; esac
case "$(uname -m)" in x86_64 | amd64) arch=x86_64 ;; *) arch=aarch64 ;; esac
asset="sbx-$os-$arch.tar.gz"
sha() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

# 가짜 릴리스: sbx(버전을 찍는 스크립트)와 sbx-shim.
release() { # release <디렉터리> [shim 없이: noshim]
    local d=$1 p="$work/pkg/sbx-$os-$arch"
    rm -rf "$work/pkg" && mkdir -p "$p" "$d"
    printf '#!/bin/sh\necho "sbx 9.9.9-test"\n' >"$p/sbx"
    [ "${2:-}" = noshim ] || printf '#!/bin/sh\n' >"$p/sbx-shim"
    chmod 755 "$p"/*
    tar -czf "$d/$asset" -C "$work/pkg" "sbx-$os-$arch"
    echo "$(sha "$d/$asset")  $asset" >"$d/$asset.sha256"
}

# 가짜 cosign: 받은 인자를 적고 FAKE_COSIGN_EXIT로 끝난다.
mkdir -p "$work/cosign-bin"
cat >"$work/cosign-bin/cosign" <<'EOF'
#!/bin/sh
printf '%s\n' "$@" >"$FAKE_COSIGN_ARGS"
exit "${FAKE_COSIGN_EXIT:-0}"
EOF
chmod 755 "$work/cosign-bin/cosign"
# cosign이 없는 PATH. 이 머신에 진짜 cosign이 그 안에 있으면 "없음" 경우는 건너뛴다.
plain="/usr/bin:/bin:/usr/sbin:/sbin"
no_cosign=true
PATH="$plain" command -v cosign >/dev/null 2>&1 && no_cosign=false

pass=0
# run <이름> <기대: ok|fail> <출력에 있어야 할 글> <PATH> [환경...]
run() {
    local name=$1 want=$2 needle=$3 path=$4; shift 4
    local dir="$work/install-$pass" out
    rm -rf "$dir"
    set +e
    out=$(env -i HOME="$work/home" PATH="$path" FAKE_COSIGN_ARGS="$work/cosign-args" \
        SBX_DOWNLOAD_URL="file://$work/rel" SBX_INSTALL_DIR="$dir" "$@" sh "$root/install.sh" 2>&1)
    local code=$?
    set -e
    local got=ok; [ $code -eq 0 ] || got=fail
    if [ "$got" != "$want" ] || ! grep -qF -- "$needle" <<<"$out"; then
        echo "FAIL $name: wanted $want with '$needle', got $got (exit $code):"; echo "$out"; exit 1
    fi
    if [ "$want" = ok ]; then
        "$dir/sbx" | grep -q 9.9.9-test || { echo "FAIL $name: sbx not installed"; exit 1; }
        [ -x "$dir/sbx-shim" ] || { echo "FAIL $name: sbx-shim not installed"; exit 1; }
        for f in "$dir"/*.new; do [ ! -e "$f" ] || { echo "FAIL $name: left $f"; exit 1; }; done
    else
        [ ! -e "$dir/sbx" ] || { echo "FAIL $name: installed despite failing"; exit 1; }
    fi
    pass=$((pass + 1))
    echo "ok   $name"
}

release "$work/rel"
with="$work/cosign-bin:$plain"
if $no_cosign; then
    run "no cosign, auto: checksum only" ok "only the SHA-256 was checked" "$plain"
    run "no cosign, require: refused" fail "needs cosign" "$plain" SBX_VERIFY=require
fi
run "off: no signature check" ok "installed" "$with" SBX_VERIFY=off FAKE_COSIGN_EXIT=1
run "bad SBX_VERIFY value" fail "must be auto, require or off" "$with" SBX_VERIFY=yes
# cosign이 있으면 서명이 없는 것은 오류다(URL을 쥔 쪽이 서명만 지울 수 있다). 모든 릴리스가 서명돼 있다.
run "auto, signature removed: refused" fail "no signature" "$with"
run "auto, unsigned release by name: refused" fail "no signature" "$with" SBX_VERSION=v0.2.0
run "require, unsigned release: refused" fail "no signature" "$with" SBX_VERIFY=require SBX_VERSION=v0.2.0
run "bad SBX_VERSION: refused" fail "must be a release tag" "$with" SBX_VERSION='v1;rm -rf x'
run "bad SBX_VERSION (not a tag): refused" fail "must be a release tag" "$with" SBX_VERSION=latest
echo '{}' >"$work/rel/$asset.sigstore.json"
run "auto, signature that does not verify: refused" fail "does not verify" "$with" FAKE_COSIGN_EXIT=1
run "require, signature that does not verify: refused" fail "does not verify" "$with" SBX_VERIFY=require FAKE_COSIGN_EXIT=1
run "auto, good signature" ok "verified the Sigstore signature" "$with"
# cosign에 넘긴 신원: 이 저장소의 release.yml, v* 태그, GitHub Actions 발급자.
args=$(cat "$work/cosign-args")
grep -qxF '^https://github[.]com/toollyst/sandbox/[.]github/workflows/release[.]yml@refs/tags/v' <<<"$args" \
    || { echo "FAIL: cosign identity regexp: $args"; exit 1; }
grep -qxF 'https://token.actions.githubusercontent.com' <<<"$args" || { echo "FAIL: cosign issuer: $args"; exit 1; }
grep -qxF -- '--certificate-identity-regexp' <<<"$args" || { echo "FAIL: cosign flags: $args"; exit 1; }
run "require, good signature" ok "verified the Sigstore signature" "$with" SBX_VERIFY=require
# 버전을 고르면 그 태그에서 온 서명이어야 한다(옛 서명본으로 바꿔치지 못하게). 정규식의 . 과 +는 글자 그대로다.
run "by name: the exact tag" ok "verified the Sigstore signature" "$with" SBX_VERSION=v0.2.0+build.1
grep -qxF '^https://github[.]com/toollyst/sandbox/[.]github/workflows/release[.]yml@refs/tags/v0[.]2[.]0[+]build[.]1$' "$work/cosign-args" \
    || { echo "FAIL: exact tag regexp: $(cat "$work/cosign-args")"; exit 1; }

# 체크섬이 틀리면(서명보다 먼저) 거부한다.
echo "0000000000000000000000000000000000000000000000000000000000000000  $asset" >"$work/rel/$asset.sha256"
run "checksum mismatch: refused" fail "checksum mismatch" "$with" SBX_VERIFY=off
rm "$work/rel/$asset.sha256"
run "missing checksum file: refused" fail "download failed" "$with" SBX_VERIFY=off
# 묶음에 sbx-shim이 없으면 거부한다.
release "$work/rel" noshim
run "archive without sbx-shim: refused" fail "does not contain sbx and sbx-shim" "$with" SBX_VERIFY=off
echo "install.sh: $pass cases passed"
