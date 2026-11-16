#!/usr/bin/env bash
# install.sh를 가짜 릴리스(file://), 가짜 cosign, 가짜 sbx로 돌려 본다. 네트워크도 진짜 cosign도 실행기 저장소도 쓰지
# 않는다. 확인하는 것: 버전 표시(--version, 첫 줄), 체크섬, SBX_VERIFY, 서명과 그 태그, 묶음 내용, 그리고 핀에 적힌
# 실행기를 까는 분기(가짜 sbx가 받은 `executor ...` 명령을 적는다. 핀은 이 스크립트가 사본에 적어 넣는다).
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
# 스크립트의 핀(installer.env에서 `cargo xtask installer`가 쓴 것).
pin() { sed -n "s/^$1=//p" "$root/install.sh"; }
installer_version=$(pin installer_version)
pin_sbx=$(pin pin_sbx)
[ -n "$installer_version" ] && [ -n "$pin_sbx" ] || { echo "FAIL: no pins in install.sh"; exit 1; }

# 가짜 릴리스: sbx(버전을 찍고, `executor ...`는 받은 인자를 FAKE_SBX_LOG에 적는 스크립트)와 sbx-shim.
release() { # release <디렉터리> [shim 없이: noshim]
    local d=$1 p="$work/pkg/sbx-$os-$arch"
    rm -rf "$work/pkg" && mkdir -p "$p" "$d"
    cat >"$p/sbx" <<'EOF'
#!/bin/sh
if [ "${1:-}" = executor ]; then
    printf '%s\n' "$*" >>"$FAKE_SBX_LOG"
    if [ "${2:-}" = current ]; then
        [ -n "${FAKE_CURRENT:-}" ] || exit 1
        echo "$FAKE_CURRENT (default (sbx executor use))"
        exit 0
    fi
    exit "${FAKE_SBX_EXIT:-0}"
fi
echo "sbx 9.9.9-test"
EOF
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

# 핀에 실행기를 적은 사본(설치 스크립트 버전 하나가 실행기를 고정한 경우).
pinned="$work/install-pinned.sh"
sed 's/^pin_executor=.*/pin_executor=wick@1.0+wick.2/; s/^pin_adapter=.*/pin_adapter=fc@2.0/' "$root/install.sh" >"$pinned"
grep -q '^pin_executor=wick@1.0+wick.2$' "$pinned" || { echo "FAIL: could not pin an executor"; exit 1; }

pass=0
script="$root/install.sh"
log="$work/sbx-log"
# run <이름> <기대: ok|fail> <출력에 있어야 할 글> <PATH> [환경...]. 설치 스크립트는 $script.
run() {
    local name=$1 want=$2 needle=$3 path=$4; shift 4
    local dir="$work/install-$pass"
    rm -rf "$dir" "$log"
    : >"$log"
    set +e
    out=$(env -i HOME="$work/home" PATH="$path" FAKE_COSIGN_ARGS="$work/cosign-args" FAKE_SBX_LOG="$log" \
        SBX_DOWNLOAD_URL="file://$work/rel" SBX_INSTALL_DIR="$dir" "$@" sh "$script" 2>&1)
    local code=$?
    set -e
    local got=ok; [ $code -eq 0 ] || got=fail
    if [ "$got" != "$want" ] || ! grep -qF -- "$needle" <<<"$out"; then
        echo "FAIL $name: wanted $want with '$needle', got $got (exit $code):"; echo "$out"; exit 1
    fi
    # 첫 줄은 설치 스크립트의 버전과 깔 것이다.
    if ! head -n1 <<<"$out" | grep -qF "sbx-install: installer $installer_version: sbx $pin_sbx"; then
        echo "FAIL $name: first line is not the installer version:"; echo "$out"; exit 1
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
# executor_calls <이름> <기대하는 `executor ...` 줄들(\n으로)>: 가짜 sbx가 받은 실행기 명령.
executor_calls() {
    local want got
    want=$(printf '%b' "$2")
    got=$(cat "$log")
    [ "$got" = "$want" ] || { echo "FAIL $1: executor calls:"; echo "$got"; echo "--- wanted:"; echo "$want"; exit 1; }
}

release "$work/rel"
with="$work/cosign-bin:$plain"

# --version: 깔지 않고 보인다. 모르는 인자는 거부한다.
v=$(env -i PATH="$plain" HOME="$work/home" SBX_INSTALL_DIR="$work/v" sh "$root/install.sh" --version)
[ "$v" = "installer $installer_version: sbx $pin_sbx, no executor pinned (sbx installs the default one on first use)" ] \
    || { echo "FAIL --version: $v"; exit 1; }
v=$(env -i PATH="$plain" HOME="$work/home" sh "$pinned" --version)
[ "$v" = "installer $installer_version: sbx $pin_sbx, executor wick@1.0+wick.2 (adapter fc@2.0)" ] \
    || { echo "FAIL --version (pinned): $v"; exit 1; }
[ ! -e "$work/v" ] || { echo "FAIL --version installed something"; exit 1; }
env -i PATH="$plain" HOME="$work/home" sh "$root/install.sh" --bogus >/dev/null 2>&1 && { echo "FAIL: unknown argument accepted"; exit 1; }
pass=$((pass + 1)); echo "ok   --version shows the pins and installs nothing"

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
# cosign에 넘긴 신원: 이 저장소의 release.yml, 고정한 sbx 버전의 태그 그대로, GitHub Actions 발급자.
args=$(cat "$work/cosign-args")
tag_re="v$(printf '%s' "$pin_sbx" | sed 's/[.]/[.]/g; s/[+]/[+]/g')\$"
grep -qxF "^https://github[.]com/toollyst/sandbox/[.]github/workflows/release[.]yml@refs/tags/$tag_re" <<<"$args" \
    || { echo "FAIL: cosign identity regexp: $args"; exit 1; }
grep -qxF 'https://token.actions.githubusercontent.com' <<<"$args" || { echo "FAIL: cosign issuer: $args"; exit 1; }
grep -qxF -- '--certificate-identity-regexp' <<<"$args" || { echo "FAIL: cosign flags: $args"; exit 1; }
run "require, good signature" ok "verified the Sigstore signature" "$with" SBX_VERIFY=require
# 다른 버전을 고르면 그 태그에서 온 서명이어야 한다(옛 서명본으로 바꿔치지 못하게). 정규식의 . 과 +는 글자 그대로다.
run "by name: the exact tag" ok "instead of $pin_sbx" "$with" SBX_VERSION=v0.2.0+build.1
grep -qxF '^https://github[.]com/toollyst/sandbox/[.]github/workflows/release[.]yml@refs/tags/v0[.]2[.]0[+]build[.]1$' "$work/cosign-args" \
    || { echo "FAIL: exact tag regexp: $(cat "$work/cosign-args")"; exit 1; }

# 실행기. 핀이 비어 있으면 깔지 않고 첫 실행 때 받는다고 알린다.
run "no executor pinned: none installed" ok "sbx installs the newest default executor" "$with" SBX_VERIFY=off
executor_calls "no executor pinned" ""
# 핀이 있으면 어댑터부터 공식 저장소에서 깐다.
script=$pinned
run "pinned executor and adapter are installed" ok "installing the executor wick@1.0+wick.2" "$with" SBX_VERIFY=off
executor_calls "pinned" "executor current\nexecutor adapter install --repo official fc@2.0\nexecutor install --repo official wick@1.0+wick.2"
run "SBX_EXECUTOR=none: no executor" ok "SBX_EXECUTOR=none" "$with" SBX_VERIFY=off SBX_EXECUTOR=none
executor_calls "SBX_EXECUTOR=none" ""
run "SBX_EXECUTOR set: yours is used" ok "SBX_EXECUTOR is set" "$with" SBX_VERIFY=off SBX_EXECUTOR=/opt/mine
executor_calls "SBX_EXECUTOR set" ""
# 사용자가 고른 다른 실행기는 그대로 둔다.
run "another executor in use: kept" ok "keeping your executor other@3" "$with" SBX_VERIFY=off FAKE_CURRENT=other@3
executor_calls "another executor" "executor current"
# 같은 실행기의 옛 버전이 기본이었으면 고정한 버전이 그 자리를 갖는다.
run "older pinned executor: replaced as default" ok "now using wick@1.0+wick.2 (was wick@0.9)" "$with" SBX_VERIFY=off FAKE_CURRENT=wick@0.9
executor_calls "older" "executor current\nexecutor adapter install --repo official fc@2.0\nexecutor install --repo official wick@1.0+wick.2\nexecutor use wick@1.0+wick.2"
# 실행기를 깔지 못해도(오프라인, 저장소에 아직 없음) sbx는 깔렸고 성공이다.
run "executor install fails: sbx stays, warning" ok "could not install the adapter fc@2.0" "$with" SBX_VERIFY=off FAKE_SBX_EXIT=1
script="$root/install.sh"

# 체크섬이 틀리면(서명보다 먼저) 거부한다.
echo "0000000000000000000000000000000000000000000000000000000000000000  $asset" >"$work/rel/$asset.sha256"
run "checksum mismatch: refused" fail "checksum mismatch" "$with" SBX_VERIFY=off
rm "$work/rel/$asset.sha256"
run "missing checksum file: refused" fail "download failed" "$with" SBX_VERIFY=off
# 묶음에 sbx-shim이 없으면 거부한다.
release "$work/rel" noshim
run "archive without sbx-shim: refused" fail "does not contain sbx and sbx-shim" "$with" SBX_VERIFY=off
echo "install.sh: $pass cases passed"
