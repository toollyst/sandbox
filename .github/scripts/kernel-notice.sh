#!/usr/bin/env bash
# 어댑터 묶음에 넣는 커널 소스 고지(GPL-2.0)를 OUT에 쓴다. 값은 손으로 적지 않고 xtask/src/kernel.rs에서 읽는다
# (VERSION, SHA256, 내려받는 URL 틀). 설정 링크는 GITHUB_REPOSITORY의 GITHUB_SHA 커밋의 xtask/kernel/이다(다른
# 저장소의 워크플로에서 돌리면 둘을 toollyst/sandbox와 그 커밋으로 준다). 이 저장소의 루트에서 돌린다.
#
#   .github/scripts/kernel-notice.sh OUT
set -euo pipefail
[ $# -eq 1 ] || { echo "usage: $0 OUT" >&2; exit 2; }
rs=xtask/src/kernel.rs
ver=$(sed -n 's/^pub const VERSION: &str = "\(.*\)";$/\1/p' "$rs")
sum=$(sed -n 's/^pub const SHA256: &str = "\(.*\)";$/\1/p' "$rs")
tpl=$(grep -o 'https://cdn\.kernel\.org/[^"]*' "$rs" | head -n1)
[[ $ver =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]] || { echo "$rs: cannot read VERSION" >&2; exit 1; }
[[ $sum =~ ^[0-9a-f]{64}$ ]] || { echo "$rs: cannot read SHA256" >&2; exit 1; }
[[ $tpl == *'{VERSION}'* ]] || { echo "$rs: cannot read the source URL" >&2; exit 1; }
url=${tpl//\{VERSION\}/$ver}
base="${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:?}"
commit=${GITHUB_SHA:?}
cat >"$1" <<EOF
The file \`kernel\` in this bundle is Linux $ver, the default guest kernel of the firecracker adapter.

License:        GNU General Public License version 2 (GPL-2.0-only WITH Linux-syscall-note)
                https://www.kernel.org/doc/html/latest/process/license-rules.html
Source:         $url
SHA-256:        $sum
Configuration:  $base/tree/$commit/xtask/kernel
                (upstream Firecracker's CI config for the architecture, merged with sbx.config)
Build:          $base/blob/$commit/xtask/src/kernel.rs  (cargo xtask kernel --arch <x86_64|aarch64>)

It is built from the unmodified source above with that configuration; no patches are applied.
EOF
