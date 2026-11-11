#!/bin/sh
# 저장소 설정(.github/settings/)을 GitHub에 적용한다. 같은 이름의 ruleset은 고쳐 쓰고, 없으면 만든다.
# 메인테이너가 손으로 돌린다: .github/scripts/apply-settings.sh [owner/repo]
# 필요: gh(저장소 관리자 권한으로 로그인), jq.
#
# - ruleset-main.json: 기본 브랜치. 삭제·force-push 금지, PR과 ci 검사 통과가 필요하다(관리자는 우회할 수 있다).
# - ruleset-gh-pages.json: 설치 스크립트를 내는 gh-pages. 삭제·force-push 금지.
# - ruleset-tags.json: 릴리스 태그 v*. 관리자만 만들고 바꾸고 지운다.
# - environment-release.json: release.yml의 publish가 기다리는 승인. 승인자와, 태그 v*에서만.
set -eu
repo=${1:-toollyst/sandbox}
dir=$(dirname "$0")/../settings

for f in "$dir"/ruleset-*.json; do
    name=$(jq -r .name "$f")
    id=$(gh api "repos/$repo/rulesets" --jq ".[] | select(.name == \"$name\") | .id")
    if [ -n "$id" ]; then
        gh api -X PUT "repos/$repo/rulesets/$id" --input "$f" --jq '"updated ruleset \(.name) (\(.id))"'
    else
        gh api -X POST "repos/$repo/rulesets" --input "$f" --jq '"created ruleset \(.name) (\(.id))"'
    fi
done

gh api -X PUT "repos/$repo/environments/release" --input "$dir/environment-release.json" --jq '"environment \(.name)"'
if ! gh api "repos/$repo/environments/release/deployment-branch-policies" --jq '.branch_policies[].name' | grep -qx 'v\*'; then
    gh api -X POST "repos/$repo/environments/release/deployment-branch-policies" -f name='v*' -f type=tag --jq '"deployment policy \(.name) (\(.type))"'
fi
