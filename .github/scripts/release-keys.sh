#!/bin/sh
# 릴리스 서명 키를 한 번에 만든다: 키 4쌍, GitHub environment 비밀 값, update.rs의 공개키. 순서와 까닭은
# docs/developers/release-keys.md.
#
#   .github/scripts/release-keys.sh [--no-upload] [--force] [owner/repo]
#
# - 키는 저장소 안 release-keys/(.gitignore에 있다, 권한 0700)에 만든다: archive-1, archive-2, index-1, index-2
#   (.key는 비밀키, .pub은 공개키). 이미 있는 키는 다시 만들지 않는다.
# - archive-1을 release의 SBX_RELEASE_SIGNING_KEY로, index-1을 release와 release-index의 SBX_RELEASE_INDEX_KEY로
#   올린다. 비밀 값이 이미 있으면 멈춘다(그 키로 서명한 릴리스가 있을 수 있다). --force면 바꾼다. --no-upload면 올리지 않는다.
# - crates/app/cli/src/update.rs의 INDEX_KEYS, ARCHIVE_KEYS가 비어 있으면 공개키를 넣는다(쓰는 것, 예비 순서).
# - 끝나면 release-keys/의 .key를 오프라인 두 곳에 백업하라고 알린다. 백업한 뒤 지워도 된다(예비 키도 백업에 있어야 한다).
#
# 필요: cargo, gh(저장소 관리자로 로그인, --no-upload가 아니면). environment release-index는 apply-settings.sh가 만든다.
set -eu

upload=1 force=0 repo=toollyst/sandbox
for a in "$@"; do
    case "$a" in
        --no-upload) upload=0 ;;
        --force) force=1 ;;
        -*) echo "unknown option $a" >&2; exit 2 ;;
        *) repo=$a ;;
    esac
done

root=$(cd "$(dirname "$0")/../.." && pwd)
dir="$root/release-keys"
cd "$root"

# 비밀키가 커밋되지 않게: 디렉터리가 git에서 무시되는지 먼저 본다.
mkdir -p "$dir"
chmod 700 "$dir"
if ! git check-ignore -q "$dir/x.key"; then
    echo "release-keys/ is not ignored by git; refusing to put secret keys there" >&2
    exit 1
fi

cargo build -q -p xtask
xtask="$root/target/debug/xtask"

for n in archive-1 archive-2 index-1 index-2; do
    if [ -f "$dir/$n.key" ]; then
        echo "$n: already there"
    else
        "$xtask" repo-keygen --out "$dir/$n" >/dev/null
        echo "$n: made"
    fi
done
pub() { tr -d '[:space:]' < "$dir/$1.pub"; }

if [ "$upload" = 1 ]; then
    command -v gh >/dev/null || { echo "gh is needed to upload the keys (or pass --no-upload)" >&2; exit 1; }
    has() { gh secret list --env "$1" --repo "$repo" --json name --jq '.[].name' | grep -qx "$2"; }
    for pair in release:SBX_RELEASE_SIGNING_KEY release:SBX_RELEASE_INDEX_KEY release-index:SBX_RELEASE_INDEX_KEY; do
        env=${pair%%:*} name=${pair#*:}
        if has "$env" "$name" && [ "$force" = 0 ]; then
            echo "$name is already set in $env; pass --force to replace it (releases signed with the old key stay signed by it)" >&2
            exit 1
        fi
    done
    gh secret set SBX_RELEASE_SIGNING_KEY --env release --repo "$repo" < "$dir/archive-1.key"
    gh secret set SBX_RELEASE_INDEX_KEY --env release --repo "$repo" < "$dir/index-1.key"
    gh secret set SBX_RELEASE_INDEX_KEY --env release-index --repo "$repo" < "$dir/index-1.key"
    echo "uploaded: SBX_RELEASE_SIGNING_KEY (release), SBX_RELEASE_INDEX_KEY (release, release-index)"
fi

src=crates/app/cli/src/update.rs
if grep -qx 'pub const INDEX_KEYS: &\[&str\] = &\[\];' "$src" && grep -qx 'pub const ARCHIVE_KEYS: &\[&str\] = &\[\];' "$src"; then
    # rustfmt가 쓰는 모양 그대로(한 줄에 하나), cargo fmt --check가 통과하게.
    tmp="$src.tmp.$$"
    awk -v i1="$(pub index-1)" -v i2="$(pub index-2)" -v a1="$(pub archive-1)" -v a2="$(pub archive-2)" '
        $0 == "pub const INDEX_KEYS: &[&str] = &[];" { printf "pub const INDEX_KEYS: &[&str] = &[\n    \"%s\",\n    \"%s\",\n];\n", i1, i2; next }
        $0 == "pub const ARCHIVE_KEYS: &[&str] = &[];" { printf "pub const ARCHIVE_KEYS: &[&str] = &[\n    \"%s\",\n    \"%s\",\n];\n", a1, a2; next }
        { print }' "$src" > "$tmp"
    mv "$tmp" "$src"
    echo "public keys written to $src (commit it)"
else
    echo "$src already has keys; not changed. The public keys are:"
    for n in index-1 index-2 archive-1 archive-2; do echo "  $n: $(pub "$n")"; done
fi

cat <<EOF

Now back up the secret keys, in two offline places (an encrypted USB drive, a password manager):
  $dir/archive-1.key  $dir/archive-2.key
  $dir/index-1.key    $dir/index-2.key
Without them sbx upgrade can't move past the last release they signed. Never commit or paste them.
After the backup you may delete $dir (keep the spares in the backup).
EOF
