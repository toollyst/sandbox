# 로드맵

2026년 11월 9일 · 상위 문서: sandbox 기획서, [docs/plan/architecture.md](plan/architecture.md)

순서는 기획서의 성공 기준 1 "Linux에서 공개 OCI 이미지를 받아 microVM 안에서 명령을 실행하고 결과를 받는다"까지의 최단 경로다. 각 단계는 "어떻게 검증되면 끝인가"를 함께 적는다.

## M1. 이미지 경로 (호스트 쪽, 모든 OS)

공개 이미지를 받아 VM에 붙일 rootfs 디스크까지 만든다. VM이 필요 없으므로 macOS와 Windows에서도 동작하고 검증된다.

| 단계 | 내용 | 끝났다는 기준 | 상태 |
|---|---|---|---|
| 1 | 상태 직렬화 (이미지 기록, 태그 기록) | 저장 후 다시 읽으면 같은 값 | 완료 |
| 2 | 레지스트리 pull: Docker Hub 익명·토큰 인증, index에서 플랫폼 선택, manifest·config 해석, blob digest 검증 | `sbx pull alpine`이 Docker Hub에서 받고, digest가 틀리면 저장하지 않는다 | 완료 |
| 3 | prepare: 레이어 tar 읽기, 병합, 파일 내용 임시 저장 | 레이어 diff_id 검증, 병합 규칙 테스트 통과 | 완료 |
| 4 | erofs writer | 만든 이미지를 erofs 공식 도구(`fsck.erofs`)가 오류 없이 읽고, 풀어낸 트리가 원본과 같다 | 완료 (Linux에서 `fsck.erofs` 테스트 통과, alpine:3.20 실제 이미지를 풀어 레이어 tar와 518개 항목 일치) |
| 5 | CLI `pull`, `prepare`, `images`, `rmi` | 명령으로 위 흐름이 동작 | 완료 |
| 5a | `rmi` 후 다른 이미지가 쓰지 않는 레이어 blob 삭제 | 동시에 같은 레이어를 쓰는 이미지를 pull해도 blob이 사라지지 않는다 | 완료 (규칙: `sbx-store` `write::claim_blobs`, `write::collect_blobs`) |

## M2. 실행 경로 (Linux x64)

| 단계 | 내용 | 끝났다는 기준 | 상태 |
|---|---|---|---|
| 6 | 호스트-게스트 메시지 정의 | 프레이밍 위 인코딩, 버전 확인 | 완료 |
| 7 | 게스트 init: rootfs와 scratch를 overlay로 합침, exec, 파일 송수신 | 게스트 단독 테스트 | 완료: 연결 처리, scratch 포맷(단독 테스트, e2fsck, 커널 마운트), tty. 부팅 순서는 구현했고 실제 부팅은 외부 실행기 E2E에서 확인 |
| 8 | 게스트 init initramfs 만들기 (`xtask`), x86_64·aarch64 | `/init`이 정적 `sbx-guest`인 initramfs | 완료 (`cargo xtask guest-initramfs`, 두 아키텍처 CI에서 만든다. GNU cpio·bsdcpio로 확인). 배포 빌드(`cargo xtask dist`)는 sbx 안에 내장하고 처음 쓸 때 캐시로 꺼낸다 |
| 9 | 실행기 통신 규약(`sbx-executor-proto`)과 외부 실행기 어댑터(`sbx-executor-process`) | 규약을 지키는 `sbx-executor-fake`로 적합성 테스트 통과 | 완료 |
| 10 | 실행 코어: create, start, exec, cp, stop, rm (attached) | fake 실행기로 통과 (실제 VM은 외부 실행기로 E2E) | 완료 (fake, macOS·Linux) |
| 11 | CLI `run`, `create`, `start`, `exec`, `cp`, `stop`, `rm`, `ps`, `inspect` (뒤에 `restart`, `kill`, `wait`) | `sbx run alpine echo hi`가 `hi`를 출력 | fake 실행기로 끝-끝 완료 (`crates/app/cli/tests/e2e.rs`: 출력, 종료 코드 125/126/127, `-i`, 시그널 전달, cp, Docker식 stop(StopSignal → `-t` 뒤 끄기), kill, wait, restart). 실제 이미지·VM은 외부 실행기 E2E |

VMM은 이 저장소 밖(Firecracker, macOS·Windows에서는 같은 인터페이스를 구현한 wick)의 것이다. 이 저장소에는 실행기 규약과,
규약을 Firecracker 공개 인터페이스로 잇는 adapter(`sbx-executor-firecracker`)가 있다.

**성공 기준 1 충족 (2026-11-09, macOS arm64 HVF, wick의 전신(toollyst/firecracker) + `sbx-executor-firecracker` + erofs·vsock을 켠 6.1 커널)**:
`sbx run --rm alpine:3.20 echo hi`가 실제 microVM에서 `hi`를 출력했다(처음 pull·prepare 포함 3.0초, 이후 `run --rm ... true` 0.55초).
같은 환경에서 확인: 종료 코드(7, 127), `-i`, `--cpus`·`-m`, overlay 루트, `run -d`·`ps`·`exec`·`cp`(양방향)·`logs`,
`stop`(SIGTERM을 받아 0으로 끝남, 0.16초), 네트워크 차단(`lo`만), `--network allow`(허용 호스트로 apk·curl 200, 그 밖은 차단).
알려진 차이: busybox `wget`은 HTTPS를 프록시에 `GET https://…`(절대 형식)로 보내 400을 받는다(`CONNECT`를 쓰는 curl, apk는 된다).
Linux KVM(upstream Firecracker)과 CI 자동 실행은 남았다.

## M3. 격리와 수명

| 단계 | 내용 | 상태 |
|---|---|---|
| 12 | detached 수명과 `sbx-shim`, 고아 프로세스 정리 | 완료 (수명은 시작할 때 정한다: `run -d`, `start`/`start -a`. `logs`. `run -dit`와 `attach`(shim의 붙기 소켓, Ctrl+P Ctrl+Q로 떨어짐, Unix). shim이 죽으면 실행기 규약으로 VM이 끝나고 `stopped`. fake로 끝-끝 테스트) |
| 13 | 폴더 공유 copy, bind (bind는 실행기 capability가 있을 때) | 완료 (bind는 virtio-fs 태그로 실행기에 넘기고 게스트가 마운트. fake는 심볼릭 링크로 흉내. 실제 virtio-fs는 외부 실행기 E2E) |
| 14 | 네트워크 정책 장치 (기본 차단, 허용 방식) | 완료 (`--network allow --allow HOST:PORT`, 게스트 안 HTTP 프록시 + 호스트 허용 목록, 이름이 내부 주소로 풀리면 거부. fake와 curl로 끝-끝 테스트) |
| 15 | 정책 위반 시나리오 통합 테스트 (외부 접속, 공유 범위 밖 접근) | 정책 단위(허용 목록 거부, 내부 주소 거부, 복사 경로 탈출)는 테스트됨. 장치 없음·읽기 전용 공유의 실제 강제는 VM에서만 확인되므로 외부 실행기 E2E |

## M4. 공개 준비

| 단계 | 내용 | 상태 |
|---|---|---|
| 16 | 실행기 저장소 규약과 `sbx executor install NAME[@VERSION]`, `search`, `repo` | 완료 (`sbx-executor-repo`: 정적 호스팅 가능한 `index.json` + `.tar.gz` 묶음, SHA-256 확인. file:// 저장소에서 실제 firecracker 묶음 설치 0.18초) |
| 17 | 공식 저장소와 릴리스(CI) | 완료, 첫 릴리스 `v0.1.1`(2026-11-10. `v0.1.0`은 macos-14 러너 지원 종료로 릴리스 없이 멈췄다). `install.sh`로 macOS에 설치해 실제 VM smoke 17/17: 공식 저장소는 묶음을 GitHub Releases에, `index.json`을 GitHub Pages(gh-pages 브랜치의 `executors/`)에 둔다. sbx에 `official`로 기본 등록(지우면 남는다). `.github/workflows/release.yml`: 태그 `v<버전>`에서 sbx(Linux musl 정적, macOS arm64), 실행기 묶음(Linux는 upstream Firecracker로 만들고 KVM smoke 통과 뒤, macOS는 저장소 변수 `FC_FORK_TAG`가 있을 때 toollyst/firecracker 릴리스로), `repo-add --url`로 색인. toollyst/firecracker `v1.18.0-fork.1` 릴리스(macOS 15+, hypervisor 권한 서명)를 `FC_FORK_TAG`로 지정, 그 자산으로 macOS 단계를 재현해 실제 VM smoke 17/17(2026-11-10). Pages 소스는 gh-pages. 그 뒤 toollyst/firecracker는 toollyst/wick으로 바뀌었다: 변수는 `WICK_TAG`(wick 릴리스 태그, 예: `v1.18.0-wick.1`), 없으면 macOS 묶음 없이 릴리스한다. 그 뒤 실행기는 sbx 릴리스와 떼어 `executors.yml`로 올린다(34). |
| 18 | 커널 | 완료: 보통 이미지는 실행기 묶음의 기본 리눅스 커널(아키텍처당 하나)로, 라벨 `io.github.toollyst.sandbox.kernel`로 커널을 알린 이미지는 그 커널로(실행기 규약 2의 `custom_kernel`). 실제 VM으로 확인. 기본 커널은 `cargo xtask kernel`(Linux 6.1.155, upstream Firecracker CI 설정 + `xtask/kernel/sbx.config`)로 CI에서 빌드하고, 이 커널로 macOS HVF smoke 17/17 |
| 19 | README, 데모, 설치 한 줄, 벤치마크 | 벤치마크 완료(macOS arm64 HVF: `run --rm alpine true` p50 121ms, `exec` p50 8ms, 10개 동시 0.50초). 설치 한 줄 완료(`install.sh`: `curl -fsSL https://toollyst.github.io/sandbox/install.sh \| sh`, SHA-256 확인, `~/.local/bin`, 기본 실행기까지. 로컬 file:// 릴리스로 확인. 그 뒤 실행기는 설치하지 않게 바뀌었다(35)). README 영어 완료. 데모 완료(`cargo xtask demo`: 실제 명령을 실제 시각대로 담은 움직이는 SVG `assets/demo.svg`, 바깥 도구 없이 CSS만. 머신이 한가할 때 다시 담는다) |
| 20 | Linux KVM(upstream Firecracker) 끝-끝과 CI | x86_64 완료: CI `vm` 작업이 GitHub 러너(KVM)에서 upstream Firecracker v1.17.0 + 빌드한 커널로 smoke 17/17과 bench. 남음: aarch64(GitHub arm 러너에 /dev/kvm이 없어 건너뛴다. 묶음은 만든다) |
| 21 | AI 에이전트 입구: MCP 서버 | 완료: `sbx mcp`(stdio, JSON-RPC 직접 구현). 도구 `run`, `create`, `exec`, `write_file`, `read_file`, `list`, `remove`. 정책 없음: 인자를 CLI 인자(`--이름=값`)로 바꿔 같은 파서로 해석, 네트워크는 `allow`로만, 호스트 마운트는 내보내지 않는다. 시간 제한(기본 300초). 이미지를 받는 동안 `notifications/progress`. e2e(가짜 실행기)와 macOS 실제 VM으로 확인 |
| 22 | Docker 사용 편의 | 완료: `--name`(ID 대신 어디서나, `cp 이름:경로`도), `ps -q`·`images -q`, `ps`의 CREATED·NAMES와 새것부터, `--env-file`, `logs --tail`, `sbx completion`(bash, zsh, fish, powershell, elvish), `sbx info`(호스트, 가상화, 실행기, 디렉터리), 받기 진행 막대(터미널일 때만). e2e로 확인 |

## M5. 적대적 검토 대응 (2026-11-10)

지금은 알파다. README 맨 위와 SECURITY.md에 그렇게 적는다. 명령, 플래그, 저장 형식, 실행기 규약은 바뀔 수 있다.

게스트, 레지스트리, 에이전트를 공격자로 보고 호스트 쪽 코드를 검토했다. 고친 것마다 공격을 흉내 내는 테스트가 있다.

| 단계 | 내용 | 상태 |
|---|---|---|
| 23 | `cp` 대상 이름을 호스트가 정한다, setuid 등 버림 | 완료 (`hostfs` 적대적 게스트 테스트) |
| 24 | MCP `read_file`은 메모리로만 받고 호스트 링크를 따라가지 않는다 | 완료 (`Sandbox::read_file`) |
| 25 | MCP 도구는 MCP로 만든 샌드박스만 (`--all-sandboxes`) | 완료 (e2e) |
| 26 | 이미지 받기: realm·redirect는 인터넷 쪽·https만, 읽기 시간 제한, blob 크기 상한, `is_public_ip` 범위 보강 | 완료 (가짜 레지스트리 테스트) |
| 27 | 자원: 출력 채널 4 MiB, `main.log` 256 MiB, egress 동시 연결 256, socket 디렉터리 소유자 확인 | 완료 |
| 28 | MCP: 취소, 클라이언트가 사라지면 정리, 출력 끝 보존, 규약 오류 코드 | 완료 (e2e) |
| 29 | Firecracker adapter의 설정 파일을 공용 `/tmp`에 예측 가능한 이름으로 쓴다 (Linux 다중 사용자) | 남음: adapter 수정 중이라 그쪽에서 |
| 30 | 게스트 프로토콜 디코더에 무작위·변형 입력 20만 개 (패닉 없음, 받아들인 것은 다시 인코딩해도 같음), 프레임은 받은 만큼만 버퍼를 늘린다 | 완료 |
| 31 | MCP 자원 상한(샌드박스 수, vCPU, 메모리), 커버리지 기반 퍼징 | 남음 |

**실행기와 어댑터 분리, 버전 관리.** 실행기 묶음 하나에 어댑터, VMM, 커널을 함께 담으면 Firecracker 버전과 어댑터
버전이 한 번호에 묶이고, 다른 사람이 자기 VMM을 붙일 수 없다. 그래서 VMM 묶음(VMM만)과 그것을 돌리는 어댑터를 나누고,
CLI는 nvm, gvm처럼 버전을 고른다. 규칙은 `sbx-executor-proto`("어댑터와 VMM 묶음"), `sbx-executor-install`,
`sbx-executor-repo`(schema 2), `crates/app/cli/src/choose.rs` 문서에 있다.

| 단계 | 내용 | 상태 |
|---|---|---|
| 32 | VMM 묶음과 어댑터: VMM 묶음은 VMM만 담고(`.sbx-adapter`에 어댑터 이름), sbx가 어댑터를 `SBX_EXECUTOR_VMM_DIR`와 함께 띄운다(shim `--vmm-dir` 포함). `firecracker` 어댑터는 그 디렉터리에서 `firecracker`를 찾고 기본 커널은 자기 옆에 둔다. 저장소 schema 2(`v2/index.json`, 없으면 `index.json`. `adapters` 섹션, 버전마다 `protocol`과 `adapter` 중 하나), `cargo xtask repo-add --schema 2 --adapter-package \| --vmm-for <어댑터> --arch` | 완료 (단위 테스트) |
| 33 | CLI 버전 관리: 고르는 순서(`--executor` 경로 또는 `NAME[@VERSION]` > 샌드박스 고정 라벨 `io.github.toollyst.sandbox.executor` > `.sbx-executor` > 기본 > PATH), `executor current`, `which`, `prune [-n]`, `search`(별칭 `ls-remote`), `@latest`, `install --from --adapter`, 원격 install은 어댑터부터, 고정된 실행기 `rm`은 `-f`가 있어야, `executor adapter ls\|install\|search\|rm`. 출력 계약 자료형(`ChosenExecutor`, `InstalledAdapter`, `Pruned`) | 완료 (e2e, 가짜 실행기) |
| 34 | 공식 저장소 배포를 sbx 릴리스와 떼기: 손으로 돌리는 `executors.yml`이 upstream Firecracker 버전을 받아 `wick@<버전>`(Linux는 upstream, macOS는 toollyst/wick)과 `firecracker` 어댑터를 `executors/v2/index.json`에 올린다. `release.yml`은 sbx만 올리고, 공식 저장소의 `wick`으로 smoke를 통과해야 한다. schema 1 `index.json`(sbx 0.1.x)은 그대로 둔다 | 진행 중 (워크플로 작성 중, 첫 실행 전) |
| 35 | 설치 스크립트는 실행기를 설치하지 않고 다음 단계(`sbx executor install wick`)를 알린다. README, 개발자 문서, SECURITY.md를 맞춘다 | 진행 중 |
| 36 | 공식 저장소의 `wick`으로 macOS(HVF)와 Linux(KVM)에서 실제 VM smoke | 남음 (34의 첫 실행 뒤) |

## M6. CLI 계약과 TUI (2026-11-10~)

M5와 따로 간다. CLI가 주인이고, 사람(`sbx tui`), 스크립트, 에이전트가 같은 CLI 출력을 쓴다. 그래서 기계가 읽는
출력과 흐름을 CLI에 먼저 갖추고, TUI는 그것만 쓰는 첫 사용자로 둔다. 참고: Docker `events`(JSON Lines, `--since`,
`--filter`), kubectl `get -w`, gh `--json`·`--jq`, clig.dev(stdout은 데이터만).

| 단계 | 내용 | 상태 |
|---|---|---|
| C1 | 출력 계약: 공개 출력 자료형을 저장 형식과 분리(`crates/app/cli/src/output.rs`), 명령마다 모양(값 하나, 대상마다 JSON Lines, 받지 않음), JSON 모드 오류는 stderr에 JSON 한 줄, JSON Schema와 문서 생성(`docs/developers/cli-output.md`) | 완료 (e2e가 모든 `ps` 출력과 계약 테스트의 출력을 자료형으로 엄격히 읽는다) |
| C2 | `sbx events` (`--since`, `--until`, Docker와 같다): 기록 장치 `sbx-events`(JSON Lines, 잠금, 1 MiB 세대 둘). 실행 코어가 상태를 쓰는 자리에서 `data_dir/events/sandboxes`에(create, start, stop, remove), 관리 코어가 `cache_dir/events/images`에(pull, prepare, untag, delete) 덧붙이고 CLI가 시각 순으로 합친다. `--watch`는 두지 않는다: TUI는 시각 T를 적고 `ps` 뒤 `events --since T` | 완료 (shim이 소유한 샌드박스 포함 e2e. 소유 프로세스가 갑자기 죽으면 stop이 없다는 한계는 `DOCKER_DIFFERENCES`에) |
| C3 | `--filter KEY=VALUE`(Docker 규칙: 다른 키는 AND, 같은 키는 OR) (`ps`, `images`, `events`). 키는 `crates/app/cli/src/filter.rs`의 표가 정본, 모르는 키·값은 오류. 시각 파서는 C2에서 함께 | 완료 (e2e `filters`) |
| C4 | `--jq EXPR` (jaq 내장, gh와 같다): `--format json`일 때만, 값이나 줄마다 식을 돌려 결과마다 한 줄(문자열은 따옴표 없이). 틀린 식은 실행 전에 오류. Go 템플릿 `--format '{{…}}'`은 하지 않는다(`DOCKER_DIFFERENCES`) | 완료 (e2e `jq_shapes_json_output`). 대가: macOS arm64 release `sbx`가 11.8 MB → 15.5 MB. jaq-std 기본 함수만 쓰면 14.5 MB지만 정규식·`@base64`·날짜 함수가 빠진 반쪽 jq라 전체를 둔다 |
| C5 | 이름·ID·이미지 동적 자동완성 (`clap_complete` `unstable-dynamic`) | 완료 (`sbx completion <셸>`이 등록 스크립트를 찍고 탭마다 `COMPLETE=<셸> sbx`가 읽기만 해서 샌드박스 이름·ID, 이미지 참조, 설치한 실행기를 낸다. 규칙은 `crates/app/cli/src/complete.rs`, e2e `completion_offers_sandboxes_and_images`) |
| C6 | 로그 시각 (`logs --timestamps`, `--since`, `--format json`): `main.log` 기록 형식에 시각을 넣고 버전을 둔다 | 완료 (판 2: 머리 `sbx-log\x02`, 기록마다 받은 시각(나노초). 머리 없는 옛 파일은 읽고, 다시 시작해도 그 판으로 이어 쓴다. 시각 없는 로그에 `-t`/`--since`/`--until`은 오류. `--format json`은 줄 조각마다 `LogLine`(UTF-8 아니면 base64). e2e `logs_timestamps_since_until_and_json`) |
| T | `sbx tui`: 전체 화면(ratatui, 대체 화면). 탭 셋(샌드박스, 이미지, 이벤트)과 최근 이벤트 칸, 보이는 줄만 그리는 표와 스크롤 막대, 거르기(`/`), 상세(JSON)·도움말·확인 창. `ps -a`·`images` 뒤 `events --since 1h`를 따라 읽고 이벤트마다 목록을 다시 읽는다. 모든 동작은 같은 `sbx`를 자식으로 부르고 상태 줄에 그 명령 줄을 보인다. 터미널이 필요한 명령(`exec -it`, `attach`, `logs -f`, `:` 아무 명령)은 대체 화면을 닫고 터미널을 넘겼다가 Enter로 돌아온다 | 완료 (규칙은 `crates/app/cli/src/tui/`. 단위 테스트가 키→명령 줄, 가상 스크롤, 그리기(TestBackend)를 보고, pty 위에서 바깥 `run -d`가 바로 보이는지, stop·rm·logs·exec·`:`·창 크기·종료 뒤 자식이 남지 않는지 확인했다. 스파이크: iocraft는 ScrollView가 가상 스크롤이 아니고 창·다이얼로그를 직접 만들어야 해서, 전체 화면 앱에는 ratatui를 골랐다. 대가 +0.3 MB) |

## 그 뒤

- 다른 외부 실행기 (libkrun 등). 규약(`sbx-executor-proto`)만 지키면 된다.
- 언어 바인딩
- OCI 범위 확대 (`docs/plan/architecture.md` 3장의 "나중")
