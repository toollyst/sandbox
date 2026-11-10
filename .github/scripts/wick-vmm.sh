#!/usr/bin/env bash
# toollyst/wick 릴리스 TAG(예: v1.18.0+wick.1)의 OS/ARCH 산출물에서 VMM 묶음 디렉터리 OUT을 만든다:
# firecracker, LICENSE, NOTICE, 그리고 릴리스에 있으면 jailer(Linux). 산출물은 wick-<TAG>-<OS>-<ARCH>.tgz(.sha256.txt로 확인)이고, 안에
# release-<TAG>-<OS>-<ARCH>/{firecracker-<TAG>-<OS>-<ARCH>,LICENSE,NOTICE}가 있다(wick의 tools/package-release.sh).
# macOS 바이너리는 hypervisor 권한으로 서명돼 있어야 한다. gh(GH_TOKEN)가 필요하다. 받은 파일은 WORK(기본: 임시
# 디렉터리)에 둔다. executors.yml이 쓴다.
#
#   .github/scripts/wick-vmm.sh TAG OS ARCH OUT      # 예: v1.18.0+wick.1 darwin aarch64 vmm
set -euo pipefail
[ $# -eq 4 ] || { echo "usage: $0 TAG OS ARCH OUT" >&2; exit 2; }
tag=$1 os=$2 arch=$3 out=$4
[[ $tag =~ ^v[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]*)?$ ]] || { echo "TAG must be a wick release tag starting with vX.Y.Z (got '$tag')" >&2; exit 2; }
[[ $os == linux || $os == darwin ]] || { echo "OS must be linux or darwin (got '$os')" >&2; exit 2; }
[[ $arch == x86_64 || $arch == aarch64 ]] || { echo "ARCH must be x86_64 or aarch64 (got '$arch')" >&2; exit 2; }
name="wick-$tag-$os-$arch"
work=${WORK:-$(mktemp -d)}
mkdir -p "$work" "$out"
(cd "$work" && gh release download "$tag" --repo toollyst/wick --pattern "$name.tgz" --pattern "$name.tgz.sha256.txt" \
   && shasum -a 256 -c "$name.tgz.sha256.txt" && tar xzf "$name.tgz")
rel="$work/release-$tag-$os-$arch"
install -m 755 "$rel/firecracker-$tag-$os-$arch" "$out/firecracker"
if [ "$os" = darwin ]; then codesign --verify --strict "$out/firecracker"; fi
if [ -f "$rel/jailer-$tag-$os-$arch" ]; then install -m 755 "$rel/jailer-$tag-$os-$arch" "$out/jailer"; fi
for f in LICENSE NOTICE; do
    if [ -f "$rel/$f" ]; then
        cp "$rel/$f" "$out/"
    elif [ -f "$work/$f" ]; then
        cp "$work/$f" "$out/"
    else
        echo "$name.tgz has no $f" >&2
        exit 1
    fi
done
