# 새 버전 알림과 `sbx upgrade` 기획

2026년 11월 12일 · 대상 릴리스 `v0.2.2` · 상위 문서: [architecture.md](architecture.md), [로드맵](../roadmap.md) M8

이 문서는 방향과 범위만 다룬다. 구현하면 규칙의 정본은 코드(문서 주석, 타입, 테스트)로 옮기고, 이 문서는 방향이
바뀔 때만 고친다.

## 1. 목표

- `sbx`가 시작할 때 더 높은 버전이 있으면 `sbx 0.2.1 (0.2.2 available)`처럼 알린다. 명령을 늦추지 않는다.
- `sbx upgrade` 한 번으로 `sbx`와 `sbx-shim`을 서명을 확인한 새 버전으로 바꾼다. 설치 스크립트를 다시 돌릴 필요가 없다.
- 지금 쓰는 버전이 거둔(yanked) 버전이거나 보안 고침이 나왔으면 더 강하게 알린다.

하지 않는 것: 자동 설치(묻지 않고 바꾸기), 실행기·어댑터 갱신(`sbx executor install`의 일), 패키지 관리자(Homebrew,
cargo install 등)로 설치한 바이너리 바꾸기(그 관리자의 명령을 알려 준다), 사용 통계.

참고한 것: gh(하루 한 번, `GH_NO_UPDATE_NOTIFIER`, 패키지 빌드에서는 끔), uv `self update`(독립 설치본만, 영수증으로
판별), deno·bun `upgrade`, rustup `self update`, npm update-notifier, `self-replace` 크레이트(Windows에서 실행 중인
exe 바꾸기), TUF(timestamp의 기한, 롤백·동결 막기), 이미 있는 실행기 저장소 색인의 서명·`serial`·`expires`(P5).

## 2. 정한 것 (2026-11-12)

| 질문 | 결정 |
|---|---|
| 새 버전을 어디서 아나 | gh-pages의 서명한 `releases.json`. GitHub API는 쓰지 않는다(호출 한도, 거둔 버전·알림을 못 담음) |
| 확인을 기본으로 하나 | 한다. 하루 한 번, 뒤에서, 끌 수 있다(`SBX_NO_UPDATE_CHECK=1`) |
| 명령 이름 | `sbx upgrade`. `docker update`는 컨테이너 자원을 바꾸는 명령이라 `update`는 비워 둔다 |
| 받은 바이너리 확인 | 공식 minisign 키 서명이 필수(공개키는 `sbx` 안에), cosign이 있으면 Sigstore 번들도 본다 |

## 3. 릴리스 색인 `releases.json`

`https://toollyst.github.io/sandbox/releases.json`과 `releases.json.minisig`. release.yml의 publish가 gh-pages의
지금 파일에 새 항목을 더하고 서명해 올린다(`install.sh`를 올리는 단계와 같은 곳).

```json
{
  "schema": 1,
  "serial": 1763000000,
  "expires": 1765600000,
  "releases": [
    {
      "version": "0.2.2",
      "security": false,
      "yanked": false,
      "advisory": null,
      "layout_version": 1,
      "protocol": { "min": 2, "max": 2 },
      "assets": {
        "linux-x86_64": { "url": "https://github.com/toollyst/sandbox/releases/download/v0.2.2/sbx-linux-x86_64.tar.gz", "sha256": "…", "size": 4718592 }
      }
    }
  ]
}
```

- 서명 범위 안의 `serial`(본 것보다 낮으면 거부)과 `expires`(지나면 거부)로 옛 색인을 다시 내미는 것과 새 색인을
  감추는 것을 막는다. 본 `serial`은 `data_dir/sbx-releases.seen`. 코드는 실행기 저장소의 `trust`(minisign 확인,
  `Seen`)를 그대로 쓴다.
- `security`: 그 버전이 보안 고침을 담는다(그 태그의 CHANGELOG 절이 GHSA나 CVE를 말하면 publish가 켠다). 강화만 적은
  Security 절로는 켜지 않는다(매 명령 알림이 흔해지지 않게). `yanked`·`advisory`는
  P12와 같은 규칙(알림 문구는 제어 문자를 빼고 512자, `index::advisory_text`).
- `layout_version`·`protocol`은 `upgrade`가 바꾸기 전에 보는 것(5장). `sha256`은 묶음을 받은 뒤 서명과 함께 본다.
- 정식 릴리스만 싣는다(`-rc` 등은 싣지 않는다).
- `expires`(Unix 초, 실행기 색인과 같다)는 30일. 매주 다시 서명하는 작업 `releases-resign.yml`(environment
  `release-index`: 승인 없이, main에서만).
  기한이 지나면 알림은 조용히 건너뛰고, `upgrade`는 이유를 말하고 멈춘다.
- 거두기: 메인테이너가 오프라인 색인 키로 `cargo xtask release-yank`를 하고 gh-pages에 push한다. 매주 다시 서명하는
  작업은 내용을 바꾸지 않고, 색인 키로 서명된 파일만 다시 서명한다(gh-pages를 고친 것만으로는 아무것도 서명되지
  않는다). xtask는 있던 `serial`이 지금보다 하루 넘게 앞서면 거부하고, 클라이언트는 `serial > expires`인 색인을 거부한다.
- 형식은 계약이다(COMPATIBILITY.md에 더한다): 필드는 더하기만, 모르는 필드는 무시, `schema`를 올리면 옛 클라이언트는
  알림을 건너뛴다.

## 4. 시작할 때 버전 확인

**언제 확인하나.** 아래를 모두 만족할 때만 네트워크에 닿는다.

- 릴리스 빌드다. sbx-build.yml이 `SBX_RELEASE_BUILD=1`로 빌드한 바이너리만 확인한다. 소스 빌드, `cargo xtask install`,
  테스트는 확인하지 않는다(시험은 디버그 빌드의 `SBX_TEST_*`로). 패키지 관리자 빌드는 빌드 때
  `SBX_UPGRADE_COMMAND='brew upgrade sbx'`처럼 주면 확인은 하되 `upgrade` 대신 그 명령을 권한다(gh의 `updaterEnabled`와
  같은 생각).
- `SBX_NO_UPDATE_CHECK`가 없고, `CI`가 없다.
- 명령이 `mcp`, `completion`, 셸 완성(`COMPLETE=`), `sbx-shim`, 내부 명령, `upgrade`가 아니다. `tui`는 머리줄에 보인다.
- 마지막 확인에서 24시간이 지났다(실패했으면 1시간 뒤 다시). 기록은 `cache_dir/update-check.json`(확인한 시각, 받은
  색인의 필요한 부분). 캐시를 쓸 수 없으면(읽기 전용) 확인하지 않는다.

**어떻게 확인하나.** 명령을 시작할 때 스레드 하나가 색인을 받는다(연결 2초, 전체 3초, 64 KiB 상한, https만, 같은
호스트 redirect만). 명령이 먼저 끝나면 시작에서 3초가 될 때까지만 기다리고 버린다. 하루에 한 번만 생기는 지연이다.
보내는 것은 요청 하나와 `User-Agent: sbx/0.2.1 (linux; x86_64)`뿐이다. ID, 명령, 사용량은 보내지 않는다.

**무엇을 보이나.** 색인에서 지금보다 높고 거두지 않은 가장 높은 버전이 있을 때.

| 상황 | stderr 한 줄 (명령 출력 뒤, stderr가 터미널일 때) | 얼마나 자주 |
|---|---|---|
| 새 버전 | `sbx 0.2.1 (0.2.2 available): run 'sbx upgrade'` | 하루 한 번 |
| 그 사이에 보안 고침 | `sbx 0.2.1 (0.2.2 available, security fix): run 'sbx upgrade'` | 명령마다 |
| 지금 버전을 거둠 | `sbx 0.2.1 (yanked: <알림>; 0.2.2 available): run 'sbx upgrade'` | 명령마다 |
| 패키지 관리자 빌드 | `… run 'brew upgrade sbx'`(`SBX_UPGRADE_COMMAND`) | 위와 같다 |

`--format json`, `-q`이면 줄을 찍지 않는다. 같은 정보를 보는 곳:

- `sbx --version`: stdout이 터미널이면 `sbx 0.2.1 (0.2.2 available)`, 아니면 지금처럼 `sbx 0.2.1`(스크립트가 읽는
  모양은 그대로). 캐시만 본다.
- `sbx info`: 첫 줄이 같은 모양. JSON에 `update`(`latest`, `security`, `yanked`, `checked_at`, 없으면 `null`)를 더한다.
- `sbx tui`: 머리줄 ` sbx 0.2.1 (0.2.2) `(보안 고침·거둠이면 빨강). `V`가 `sbx upgrade`(터미널을 넘긴다).
- `sbx upgrade --check`: 캐시를 무시하고 지금 확인한다. 종료 코드 0(최신), 10(새 버전 있음), 1(확인 실패, `sbx`의 다른 실패와 같다).

## 5. `sbx upgrade`

```
sbx upgrade [--check] [--version VERSION] [-y|--yes] [--allow-downgrade]
```

1. **설치 방식을 본다.** 이 바이너리가 있는 디렉터리에 `sbx`와 `sbx-shim`이 함께 있고 사용자가 쓸 수 있으며, 패키지
   관리자 경로(Homebrew `Cellar`, `/nix/store`, `~/.cargo/bin`, `/usr/bin` 등)가 아니어야 한다. 아니면 무엇으로
   올릴지 알려 주고 멈춘다(`SBX_UPGRADE_COMMAND`가 있으면 그 명령).
2. **목표를 고른다.** 기본은 거두지 않은 가장 높은 버전. 다른 minor로 넘어가면(0.x에서는 깨질 수 있다) CHANGELOG의
   Breaking 절 링크를 보이고 묻는다(`-y`면 묻지 않는다). `--version`은 그 버전. 낮은 버전은 `--allow-downgrade`가
   있어야 하고, 목표의 `layout_version`이 지금 데이터 디렉터리보다 낮으면 거부한다(옛 `sbx`는 새 배치를 읽지 않는다).
3. **실행기와 맞는지 본다.** 설치한 실행기·어댑터의 규약 범위가 목표의 `protocol`과 겹치지 않으면 무엇이 멈출지
   보이고 묻는다.
4. **받는다.** 색인이 가리키는 이 플랫폼의 tar.gz(Windows도 tar.gz를 함께 낸다), `.minisig`, `.sigstore.json`.
   크기 상한 256 MiB, 시간 상한.
5. **확인한다.** 색인의 `sha256`과 크기. 묶음 키의 minisign 서명(필수, 공개키는 `sbx` 안에, 여럿을 둘 수 있어
   교체할 수 있다). `cosign`이 PATH에 있으면 Sigstore 번들도 본다(신원은 `release.yml@refs/tags/v<그 버전>`, `install.sh`와
   같다. `SBX_VERIFY=require`면 cosign이 없을 때 멈춘다). 하나라도 틀리면 아무것도 바꾸지 않는다.
6. **바꾸기 전에 돌려 본다.** 같은 디렉터리의 임시 이름으로 풀고 `sbx --version`이 목표 버전을 말하는지 본다.
7. **바꾼다.** 잠금(`<디렉터리>/.sbx-upgrade.lock`) 아래에서 `sbx-shim`, 그다음 `sbx`를 rename으로 바꾼다. 하나가
   실패하면 바꾼 것을 되돌린다.
   - Unix: 실행 중인 바이너리 위로 rename해도 된다(돌고 있는 프로세스는 옛 파일을 계속 쓴다).
   - Windows: 실행 중인 exe는 덮어쓸 수 없지만 이름은 바꿀 수 있다. 옛 파일을 `.sbx.exe.old-<pid>`로 옮기고 새 파일을
     넣는다. detached 샌드박스의 `sbx-shim.exe`도 같다. 지울 수 없는 `.old-*`는 다음 `upgrade`가 지운다.
8. **알린다.** `sbx 0.2.1 → 0.2.2`, 그 버전 CHANGELOG 링크. 데이터 배치 이전은 새 `sbx`를 처음 실행할 때 한다(이미
   있는 `layout-version`).

돌고 있는 샌드박스는 그대로 둔다. 게스트 규약은 더하기만 하므로 새 `sbx`가 옛 게스트와 통신한다(0.2.1에서 확인한 것과
같다).

## 6. 보안

- 알림 경로는 서명한 색인만 믿는다. 서명이 없거나 틀리거나 기한이 지난 색인은 버린다(가짜 "보안 고침" 알림을 막는다).
- `upgrade`는 묶음의 minisign 서명이 필수다. 색인을 바꿔치기해도 묶음 서명은 못 만든다. 공개키는 바이너리 안에만
  있고 환경 변수로 바꿀 수 없다(시험용 키는 `cfg(test)`와 시험용 feature에서만).
- 키는 역할이 둘이다(TUF의 targets와 timestamp처럼). **묶음 키**(`SBX_RELEASE_SIGNING_KEY`)는 묶음만 서명하고
  environment `release`(승인, 태그만)에만 있다. **색인 키**(`SBX_RELEASE_INDEX_KEY`)는 `releases.json`만 서명하고
  `release`와 `release-index`(매주 다시 서명)에 있다. 색인 키가 새면 가짜 알림은 만들 수 있지만 설치할 묶음은 못
  만든다. 각 역할에 예비 키를 하나씩 `sbx`에 넣어 두고, 비밀 키는 비밀 값과 오프라인 보관본에만 둔다. 교체는 새
  키를 넣은 `sbx`를 먼저 내고 다음 릴리스부터 새 키로 서명한다.
- SECURITY.md 위협 모델에 "호스트의 버전 확인" 줄을 더한다: 무엇을 보내나, 무엇을 믿나, 어떻게 끄나.
- 적대적 검토 대상: 롤백(낮은 `serial`), 동결(기한), 다른 플랫폼의 묶음, 묶음 안 경로 탈출과 링크, 큰 응답, redirect,
  잠금 경합, 중간 실패 뒤 되돌리기, Windows의 `.old` 처리, 서명 없이 시험용 키가 들어간 릴리스 빌드.

## 7. 단계

| 단계 | 내용 | 끝나는 기준 |
|---|---|---|
| U1 | `releases.json` 형식과 확인(spec 크레이트에 두고 실행기 저장소의 `trust`를 씀), `xtask release-index`·`release-yank`, release.yml publish와 `releases-resign.yml`, 묶음마다 `.minisig` | 리허설이 만든 색인을 시험용 키로 확인, 거둔·기한 지난·낮은 serial 거부 테스트, 파싱 퍼징 대상 |
| U2 | 시작할 때 확인과 알림(4장), `--version`·`info`·`info` JSON, 출력 계약 문서 | 가짜 색인 서버로 e2e: 알림 줄, 하루 한 번, `CI`·`SBX_NO_UPDATE_CHECK`·json·mcp에서 없음, 3초 넘게 늦추지 않음 |
| U3 | `sbx upgrade`(Unix): 판별, 고르기, 받기, 확인, 시험 실행, 바꾸기, 되돌리기 | 임시 설치 디렉터리에서 바꾸기, 서명·해시 틀림과 중간 실패에서 그대로, 동시 두 개는 하나만 |
| U4 | Windows 바꾸기(`.old`), 돌고 있는 `sbx-shim.exe` | windows CI에서 실행 중인 바이너리 바꾸기 |
| U5 | tui 머리줄과 `:upgrade`, 끝나면 "tui를 다시 열면 새 버전" | tui 테스트 |
| U6 | 문서: install.md "Upgrade", usage, SECURITY.md, COMPATIBILITY.md, testing-and-releasing(키, 거두기), CHANGELOG | main push 전 문서 검사 |
| U7 | 적대적 검토(6장 목록) | 항목마다 회귀 테스트 |

사람의 일: 오프라인에서 묶음 키와 색인 키(각각 예비 하나 더)를 `cargo xtask repo-keygen`으로 만들기,
`SBX_RELEASE_SIGNING_KEY`를 `release`에, `SBX_RELEASE_INDEX_KEY`를 `release`와 `release-index`에 넣기
(`release-index`는 `.github/scripts/apply-settings.sh`가 만든다), 공개키를 코드와 문서에 싣기. 키가 없으면 U1의 서명 단계는 건너뛰고(리허설은 시험용 키), 알림과 `upgrade`는
"서명한 색인이 아직 없다"로 아무것도 하지 않는다.

0.2.1을 쓰는 사람은 이 코드가 없으므로 설치 스크립트를 한 번 더 돌려야 0.2.2가 된다. 그 뒤부터 `sbx upgrade`로 된다.
