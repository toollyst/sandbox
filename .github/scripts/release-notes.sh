#!/bin/sh
# CHANGELOG.md에서 한 릴리스의 절(`## <SECTION>`부터 다음 `## ` 앞까지, 제목 줄은 빼고)을 표준 출력에 낸다.
# 절이 없거나 비었으면 실패한다. release.yml이 태그의 릴리스 노트로 쓴다(리허설은 Unreleased).
#
#   .github/scripts/release-notes.sh v0.2.0 [CHANGELOG.md]
set -eu
[ $# -ge 1 ] || { echo "usage: $0 SECTION [CHANGELOG]" >&2; exit 2; }
section=$1 file=${2:-CHANGELOG.md}
notes=$(awk -v want="## $section" '
    $0 == want { on = 1; next }
    on && /^## / { exit }
    on { print }
' "$file" | sed -e '/./,$!d')
if [ -z "$(printf '%s' "$notes" | tr -d '[:space:]')" ]; then
    echo "$file has no '## $section' section (rename '## Unreleased' to '## $section' before tagging)" >&2
    exit 1
fi
printf '%s\n' "$notes"
