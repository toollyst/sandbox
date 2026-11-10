#!/usr/bin/env bash
# 어댑터 묶음과 VMM 묶음으로 로컬 v2 저장소(file://)를 REPO에 만들고, 그 저장소 하나만 두고
# `sbx executor install wick@<WICK_VERSION>`(어댑터는 함께 설치된다)과 `executor use`를 한 뒤 `cargo xtask smoke`를
# 돌린다. 사용자가 공식 저장소에서 받는 것과 같은 경로다. executors.yml(올리기 전에)과 ci.yml(vm)이 쓴다.
# 저장소 루트에서 돌린다. sbx의 데이터 디렉터리를 바꾼다(`official` 저장소를 빼고 `local`을 더한다).
#
#   .github/scripts/smoke-wick.sh SBX REPO ADAPTER_DIR ADAPTER_VERSION VMM_DIR WICK_VERSION
set -euo pipefail
[ $# -eq 6 ] || { echo "usage: $0 SBX REPO ADAPTER_DIR ADAPTER_VERSION VMM_DIR WICK_VERSION" >&2; exit 2; }
abs() { case $1 in /*) printf '%s\n' "$1" ;; *) printf '%s\n' "$PWD/$1" ;; esac; }
sbx=$(abs "$1") repo=$(abs "$2") adapter=$(abs "$3") av=$4 vmm=$(abs "$5") wv=$6
[ ! -e "$repo" ] || { echo "$repo already exists" >&2; exit 2; }
mkdir -p "$repo"
caps=$(mktemp)
SBX_EXECUTOR_VMM_DIR="$vmm" "$adapter/sbx-executor" capabilities >"$caps"
arch=$(jq -r .arch "$caps")
cargo xtask repo-add --repo "$repo" --schema 2 --adapter-package --bundle "$adapter" \
    --name firecracker --version "$av" --capabilities "$caps"
cargo xtask repo-add --repo "$repo" --schema 2 --vmm-for firecracker --arch "$arch" --bundle "$vmm" \
    --name wick --version "$wv"
rm -f "$caps"
"$sbx" executor repo rm official
"$sbx" executor repo add local "file://$repo"
"$sbx" executor install "wick@$wv"
"$sbx" executor use "wick@$wv"
"$sbx" executor ls
"$sbx" executor adapter ls
cargo xtask smoke --sbx "$sbx"
