#!/usr/bin/env bash
# upstream Firecracker 릴리스 v<VERSION>에서 VMM 묶음 디렉터리 OUT을 만든다: firecracker, jailer, LICENSE, NOTICE.
# jailer는 같은 릴리스의 것이다(어댑터가 root로 돌 때 firecracker를 가둔다).
# 압축은 그 릴리스의 .sha256.txt로 확인한다. LICENSE/NOTICE가 압축에 없으면 그 태그의 소스에서 받는다(Apache-2.0
# 재배포 조건). executors.yml과 ci.yml(vm)이 쓴다. 받은 파일은 WORK(기본: 임시 디렉터리)에 둔다.
#
#   .github/scripts/upstream-vmm.sh VERSION ARCH OUT      # 예: 1.18.0 x86_64 vmm
set -euo pipefail
[ $# -eq 3 ] || { echo "usage: $0 VERSION ARCH OUT" >&2; exit 2; }
ver=$1 arch=$2 out=$3
[[ $ver =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "VERSION must look like 1.18.0 (got '$ver')" >&2; exit 2; }
[[ $arch == x86_64 || $arch == aarch64 ]] || { echo "ARCH must be x86_64 or aarch64 (got '$arch')" >&2; exit 2; }
v="v$ver" name="firecracker-v$ver-$arch"
work=${WORK:-$(mktemp -d)}
mkdir -p "$work" "$out"
base="https://github.com/firecracker-microvm/firecracker/releases/download/$v"
(cd "$work" && curl -fsSLO "$base/$name.tgz" && curl -fsSLO "$base/$name.tgz.sha256.txt" \
   && sha256sum -c "$name.tgz.sha256.txt" && tar xzf "$name.tgz")
rel="$work/release-$v-$arch"
install -m 755 "$rel/$name" "$out/firecracker"
install -m 755 "$rel/jailer-$v-$arch" "$out/jailer"
for f in LICENSE NOTICE; do
    if [ -f "$rel/$f" ]; then
        cp "$rel/$f" "$out/"
    else
        curl -fsSL -o "$out/$f" "https://raw.githubusercontent.com/firecracker-microvm/firecracker/$v/$f"
    fi
done
