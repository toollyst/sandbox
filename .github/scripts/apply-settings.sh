#!/bin/sh
# 저장소 설정(.github/settings/)을 GitHub에 적용한다. 같은 이름의 ruleset은 고쳐 쓰고, 없으면 만든다.
# 메인테이너가 손으로 돌린다: .github/scripts/apply-settings.sh [owner/repo]
# 필요: gh(저장소 관리자 권한으로 로그인), jq.
#
# - ruleset-main.json: 기본 브랜치. 삭제·force-push 금지, PR과 ci 검사 통과가 필요하다(관리자는 우회할 수 있다).
# - ruleset-gh-pages.json: 설치 스크립트를 내는 gh-pages. 삭제·force-push 금지.
# - ruleset-tags.json: 릴리스 태그 v*. 관리자만 만들고 바꾸고 지운다.
# - environment-release.json: release.yml의 publish가 기다리는 승인. 승인자와, 태그 v*에서만.
# - environment-release-index.json: releases-resign.yml이 releases.json을 다시 서명하는 곳. 승인 없이(매주 돈다),
#   main에서만. 색인 키(SBX_RELEASE_INDEX_KEY)만 둔다. 묶음 키는 release에만 있다.
# - security-and-analysis.json: 비밀 스캔과 push 보호(비밀이 든 커밋은 push가 거부된다), Dependabot 보안 업데이트.
#   취약점 비공개 신고(private vulnerability reporting)도 켠다.
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
gh api -X PUT "repos/$repo/environments/release-index" --input "$dir/environment-release-index.json" --jq '"environment \(.name)"'
if ! gh api "repos/$repo/environments/release-index/deployment-branch-policies" --jq '.branch_policies[].name' | grep -qx 'main'; then
    gh api -X POST "repos/$repo/environments/release-index/deployment-branch-policies" -f name=main -f type=branch --jq '"deployment policy \(.name) (\(.type))"'
fi

# 보안 업데이트는 Dependabot 경고가 켜져 있어야 한다.
gh api -X PUT "repos/$repo/vulnerability-alerts" --silent && echo "dependabot alerts enabled"
gh api -X PATCH "repos/$repo" --input "$dir/security-and-analysis.json" \
    --jq '"secret scanning \(.security_and_analysis.secret_scanning.status), push protection \(.security_and_analysis.secret_scanning_push_protection.status), dependabot security updates \(.security_and_analysis.dependabot_security_updates.status)"'
gh api -X PUT "repos/$repo/private-vulnerability-reporting" --silent && echo "private vulnerability reporting enabled"
