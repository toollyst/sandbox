# 로드맵

2026년 11월 10일 · 상위 문서: sandbox 기획서, [docs/plan/architecture.md](plan/architecture.md)

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
| 17 | 공식 저장소와 릴리스(CI) | 완료, 첫 릴리스 `v0.1.1`(2026-11-10. `v0.1.0`은 macos-14 러너 지원 종료로 릴리스 없이 멈췄다). `install.sh`로 macOS에 설치해 실제 VM smoke 17/17: 공식 저장소는 묶음을 GitHub Releases에, `index.json`을 GitHub Pages(gh-pages 브랜치의 `executors/`)에 둔다. sbx에 `official`로 기본 등록(지우면 남는다). `.github/workflows/release.yml`: 태그 `v<버전>`에서 sbx(Linux musl 정적, macOS arm64), 실행기 묶음(Linux는 upstream Firecracker로 만들고 KVM smoke 통과 뒤, macOS는 저장소 변수 `FC_FORK_TAG`가 있을 때 toollyst/firecracker 릴리스로), `repo-add --url`로 색인. toollyst/firecracker `v1.18.0-fork.1` 릴리스(macOS 15+, hypervisor 권한 서명)를 `FC_FORK_TAG`로 지정, 그 자산으로 macOS 단계를 재현해 실제 VM smoke 17/17(2026-11-10). Pages 소스는 gh-pages. 그 뒤 toollyst/firecracker는 toollyst/wick으로 바뀌었고, 실행기는 sbx 릴리스와 떼어 `executors.yml`로 올린다(34). 지금 `release.yml`은 sbx만 올리고 `FC_FORK_TAG`·`WICK_TAG` 변수는 쓰지 않는다. |
| 18 | 커널 | 완료: 보통 이미지는 실행기 묶음의 기본 리눅스 커널(아키텍처당 하나)로, 라벨 `io.github.toollyst.sandbox.kernel`로 커널을 알린 이미지는 그 커널로(실행기 규약 2의 `custom_kernel`). 실제 VM으로 확인. 기본 커널은 `cargo xtask kernel`(Linux 6.1.155, upstream Firecracker CI 설정 + `xtask/kernel/sbx.config`)로 CI에서 빌드하고, 이 커널로 macOS HVF smoke 17/17 |
| 19 | README, 데모, 설치 한 줄, 벤치마크 | 벤치마크 완료(macOS arm64 HVF: `run --rm alpine true` p50 121ms, `exec` p50 8ms, 10개 동시 0.50초). 설치 한 줄 완료(`install.sh`: `curl -fsSL https://toollyst.github.io/sandbox/install.sh \| sh`, SHA-256 확인, `~/.local/bin`, 기본 실행기까지. 로컬 file:// 릴리스로 확인. 그 뒤 실행기는 설치하지 않게 바뀌었다(35)). README 영어 완료. 데모 완료(`cargo xtask demo`: 실제 명령을 실제 시각대로 담은 움직이는 SVG `assets/demo.svg`, 바깥 도구 없이 CSS만. 머신이 한가할 때 다시 담는다) |
| 20 | Linux KVM(upstream Firecracker) 끝-끝과 CI | x86_64 완료: CI `vm` 작업이 GitHub 러너(KVM)에서 upstream Firecracker v1.17.0 + 빌드한 커널로 smoke 17/17과 bench. 32 뒤로는 사용자와 같은 v2 경로다: 이 커밋의 어댑터와 upstream Firecracker VMM 묶음을 로컬 v2 저장소(file://)에 `firecracker@1.17.0`으로 올리고, `install.sh`로 설치한 sbx로 `sbx executor install`한 뒤 smoke(`.github/scripts/smoke-local.sh`). wick에도 공식 저장소에도 기대지 않는다. 남음: aarch64(GitHub arm 러너에 /dev/kvm이 없어 건너뛴다. 묶음은 만든다) |
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
| 29 | Firecracker adapter의 설정 파일을 공용 `/tmp`에 예측 가능한 이름으로 쓴다 (Linux 다중 사용자) | 완료: 제어 소켓 옆(sbx가 이 사용자만 쓰게 만든 socket 디렉터리)에 `<소켓>.firecracker.json`으로, 있던 것은 지우고 새 파일로만(`create_new`, 링크를 따라가지 않는다), 모드 0600. 테스트 `config_sits_beside_the_socket_and_never_follows_links` |
| 30 | 게스트 프로토콜 디코더에 무작위·변형 입력 20만 개 (패닉 없음, 받아들인 것은 다시 인코딩해도 같음), 프레임은 받은 만큼만 버퍼를 늘린다 | 완료 |
| 31 | MCP 자원 상한(샌드박스 수, vCPU, 메모리), 커버리지 기반 퍼징 | 퍼징 완료: `fuzz/`(cargo-fuzz, 본 workspace 밖)에 대상 10개(게스트 메시지, `cp` 받기, 허용 규칙, `main.log`, OCI 문서, 레이어→EROFS, 병합, 실행기 색인·이름·묶음 풀기). 패닉 없음과 함께 불변식(다시 인코딩해도 같음, 대상 밖에 쓰지 않음, 버전 전순서)을 본다. `cargo xtask fuzz`, `fuzz.yml`(push마다 대상마다 60초, 손으로 더 길게). 찾아 고친 것(각각 회귀 테스트): 퍼저가 IPv6 허용 규칙이 표기가 다르면 맞지 않음을, 대상을 만들며 cp 받기가 Windows에서 `..\x` 조각으로 대상 밖에 쓸 수 있음, 이미지 `StopSignal`의 실시간 오프셋 넘침(debug 패닉), 색인의 버전 `latest`를 받아들임을 찾았다. MCP 자원 상한도 완료(P7) |

**실행기와 어댑터 분리, 버전 관리.** 실행기 묶음 하나에 어댑터, VMM, 커널을 함께 담으면 Firecracker 버전과 어댑터
버전이 한 번호에 묶이고, 다른 사람이 자기 VMM을 붙일 수 없다. 그래서 VMM 묶음(VMM만)과 그것을 돌리는 어댑터를 나누고,
CLI는 nvm, gvm처럼 버전을 고른다. 규칙은 `sbx-executor-proto`("어댑터와 VMM 묶음"), `sbx-executor-install`,
`sbx-executor-repo`(schema 2), `crates/app/cli/src/choose.rs` 문서에 있다.

| 단계 | 내용 | 상태 |
|---|---|---|
| 32 | VMM 묶음과 어댑터: VMM 묶음은 VMM만 담고(`.sbx-adapter`에 어댑터 이름), sbx가 어댑터를 `SBX_EXECUTOR_VMM_DIR`와 함께 띄운다(shim `--vmm-dir` 포함). `firecracker` 어댑터는 그 디렉터리에서 `firecracker`를 찾고 기본 커널은 자기 옆에 둔다. 저장소 schema 2(`v2/index.json`, 없으면 `index.json`. `adapters` 섹션, 버전마다 `protocol`과 `adapter` 중 하나), `cargo xtask repo-add --schema 2 --adapter-package \| --vmm-for <어댑터> --arch` | 완료 (단위 테스트) |
| 33 | CLI 버전 관리: 고르는 순서(`--executor` 경로 또는 `NAME[@VERSION]` > 샌드박스 고정 라벨 `io.github.toollyst.sandbox.executor` > `.sbx-executor` > 기본 > PATH), `executor current`, `which`, `prune [-n]`, `search`(별칭 `ls-remote`), `@latest`, `install --from --adapter`, 원격 install은 어댑터부터(그 VMM 묶음의 저장소, 없으면 목록 순서로 처음 가진 저장소), 고정된 실행기 `rm`은 `-f`가 있어야, `executor adapter ls\|install\|search\|rm`. 출력 계약 자료형(`ChosenExecutor`, `InstalledAdapter`, `Pruned`) | 완료 (e2e, 가짜 실행기) |
| 34 | 공식 저장소 배포를 sbx 릴리스와 떼기: 손으로 돌리는 `executors.yml`이 wick 릴리스(입력 `wick`, 비우면 최신)를 받아 `wick@<wick 릴리스 버전>`(예: 태그 `v1.18.0` → `wick@1.18.0`, 수정 릴리스 `v1.18.0+wick.1` → `wick@1.18.0+wick.1`. `wick@1.18.0`은 그 주 버전의 가장 새 패치. 플랫폼마다 그 릴리스의 빌드, Linux 빌드가 없으면 같은 upstream 버전의 upstream Firecracker)과 `firecracker` 어댑터를 `executors/v2/index.json`에 올린다. `release.yml`은 sbx만 올리고, smoke는 ci.yml의 vm처럼 이 커밋의 어댑터와 upstream Firecracker로 로컬 저장소에서 한다. 그래서 sbx 릴리스는 wick 릴리스나 공식 저장소에 기대지 않고, 둘은 어느 순서로든 낼 수 있다. schema 1 `index.json`(sbx 0.1.x)은 그대로 둔다 | 워크플로 작성 완료, 첫 실행 전: wick의 첫 태그 `v1.18.0`을 기다린다. 그때까지 공식 저장소에는 `v2/index.json`이 없어 sbx가 schema 1 `index.json`(`firecracker` 0.1.1)을 읽고, `sbx executor install wick`은 실패한다 |
| 35 | 설치 스크립트는 실행기를 설치하지 않고 다음 단계(`sbx executor search`, `sbx executor install NAME`. 특정 실행기를 권하지 않는다)를 알린다. README, 개발자 문서, SECURITY.md를 맞춘다 | 완료 (스크립트와 문서). 문서에 적은 `sbx executor install wick`이 실제로 되는지는 34의 첫 실행 뒤 36에서 확인 |
| 36 | 공식 저장소의 `wick`으로 macOS(HVF)와 Linux(KVM)에서 실제 VM smoke | 남음 (34의 첫 실행 뒤) |
| 37 | sbx는 특정 VMM이나 저장소에 묶이지 않는다: 실행기가 없을 때의 안내(`run`, `executor current`, tui)와 설치 스크립트가 특정 실행기를 권하지 않고(`choose::NO_EXECUTOR`), tui `i`는 이름을 비워 둔다. README와 개발자 문서는 공개 규격(실행기 규약, 어댑터와 VMM 묶음, 저장소 형식)을 앞에 두고 wick은 예시 하나로 둔다. 저장소에 닿지 않아도 `executor search`·`install` 말고는 그대로 된다 | 완료 |
| 38 | 공식 실행기 저장소를 sbx 저장소에서 떼기: 게시(`executors.yml`, `executors-resign.yml`, `wick-vmm.sh`)와 색인(Pages)을 [toollyst/sandbox-executors](https://github.com/toollyst/sandbox-executors)로 옮긴다. 그 저장소가 sandbox의 한 커밋(입력 `sandbox`, SHA로 고정)에서 어댑터·커널·저장소 도구를 만들고 이 저장소의 `upstream-vmm.sh`·`kernel-notice.sh`·`smoke-local.sh`를 쓴다(의존은 그쪽 → 여기 한 방향). 어댑터(`crates/executor/firecracker`)와 커널 빌드는 여기 남는다(ci·release의 실제 VM 시험이 바깥 저장소 없이 돈다). `official`은 <https://toollyst.github.io/sandbox-executors/>, 0.1.x가 쓴 목록의 옛 `official` 줄은 새 공식 저장소로 읽고, 옛 URL의 schema 1 색인은 0.1.x를 위해 남긴다 | 완료 |
| 39 | README는 이 프로젝트를 처음 보는 사람용으로: 추구하는 바, OS별(macOS·Linux·Windows) 설치와 처음 쓰는 법, 플랫폼. 자세한 것은 사용자 안내서 `docs/guide/`(영어: install, usage, mcp, executors)로 옮긴다. 상대 링크와 앵커를 모두 확인 | 완료 |
| 40 | v0.2.0 릴리스(2026-11-11). 유지는 0.2부터: 0.1.x는 거둔다(GitHub Release v0.1.1, 태그 v0.1.0·v0.1.1, sandbox gh-pages의 schema 1 `executors/index.json`). 0.1.x만을 위한 코드도 뺀다(목록의 옛 `official` URL 읽기, install.sh의 서명 없는 옛 릴리스 예외, 문서의 0.1.x 안내). 저장소 규격의 schema 1(`index.json`)은 공개 규격이라 남긴다 | 완료 |

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
| T2 | `sbx tui` 실행기 탭(CLI에 더한 실행기 기능을 TUI에도): `executor ls`·`executor adapter ls`·`executor current`를 읽어 실행기와 어댑터를 한 표에(`*` 기본, `>` 지금 쓸 것, 어댑터를 쓰는 VMM 묶음), 위 줄에 지금 쓸 실행기와 까닭. 키 `u` use, `i` install(명령 줄을 채워 고쳐 친 뒤 터미널을 넘긴다), `S` search, `D` rm·adapter rm(묻는다), `P` prune(묻는다). 실행기는 이벤트가 없으므로 열 때, `executor` 명령이 끝날 때, 터미널을 돌려받을 때 다시 읽는다 | 완료 (단위 테스트: 키→명령 줄, 그리기. pty 위에서 `u`로 기본이 바뀌는 것을 확인) |

## M7. 프로덕션·상용 급 (2026-11-10~)

오픈소스로 두되, 보안 검토와 도입 심사를 통과할 수준으로 올린다. 기준은 도입하는 쪽의 질문이다: 무엇을 받는지
확인할 수 있나(공급망), 쓰는 쪽이 정한 상한을 지키나(상한), 문제가 나면 누가 얼마나 빨리 고치나(절차), 업그레이드에
무엇이 깨지나(호환성), 누가 무엇을 했나(감사). 참고한 것: Firecracker RELEASE_POLICY,
Cloud Hypervisor·Kata·gVisor의 보안 절차, microsandbox COMPATIBILITY, OpenSSF Best Practices·Scorecard·OSPS Baseline,
SLSA, TUF(색인 신선도), EU CRA. 범위는 sbx 자신이다. sbx는 실행기와 규약으로 통신만 하는 중개자라서, VMM을 가두는
법, VMM·커널·어댑터 묶음의 내용과 공급망, 호스트의 VMM 설정은 그것을 만드는 쪽의 일이고 여기서 다루지 않는다.

| 단계 | 내용 | 상태 |
|---|---|---|
| P1 | 공급망: `deny.toml`(RustSec 권고, 허용형 라이선스, crates.io만), CI `supply-chain` 작업, 모든 Actions를 커밋 SHA로 고정, dependabot(Actions·cargo) | 완료 |
| P2 | 릴리스 서명: 묶음마다 Sigstore keyless 번들, GitHub 출처 증명(SLSA build provenance), SBOM(SPDX) 증명. `install.sh`의 `SBX_VERIFY`(auto/require/off). `release.yml`을 손으로 돌리면 리허설(빌드·smoke·SBOM·서명만) | 완료 (리허설 42825468363: 묶음 셋의 Sigstore 번들이 cosign으로 확인되고, 바꾼 파일과 태그가 아닌 신원은 거부된다. `install.sh`는 cosign이 있을 때 그 리허설 번들을 태그 신원이 아니라며 거부했다. SBOM 패키지 418개. 태그 신원으로 통과하는 것과 출처 증명은 첫 태그 릴리스에서 확인) |
| P3 | 공급망 2 (sbx 바이너리만): `cargo auditable`로 바이너리에 의존성 목록, release publish를 승인이 필요한 environment로, SLSA Build L3(재사용 빌드 워크플로와 `--signer-workflow`). 실행기·어댑터·VMM 묶음의 SBOM·증명은 sbx의 일이 아니다(그 묶음을 만드는 쪽이 한다) | 남음 |
| P4 | 커버리지 기반 퍼징(cargo-fuzz): 게스트·실행기 규약 디코더, 저장소 색인과 버전, 묶음 풀기(경로 탈출), OCI manifest·index, egress 규칙. CI에서 짧게, 손으로 길게. 그 뒤 ClusterFuzzLite나 OSS-Fuzz | 일부: 대상 10개와 `fuzz.yml`(31). 게스트 init의 요청 처리, MCP 인자, ClusterFuzzLite·OSS-Fuzz는 남음 |
| P5 | 저장소 색인 서명(minisign): 저장소마다 공개키(여럿, 교체 중), 서명 범위 안의 `serial`(본 것보다 낮으면 거부)과 `expires`(롤백·동결 공격), 이름은 그것을 내는 첫 저장소의 것이고 설치한 이름은 받은 곳(`.sbx-origin`)에서만 다시 받는다(어댑터 포함). `repo add --key`/`--unsigned`, `install --repo`, `ls`·tui의 FROM, `xtask repo-keygen`·`repo-sign`, `smoke-local.sh`가 서명된 저장소로, `executors.yml`이 비밀 값이 있으면 서명, `executors-resign.yml` | 완료. 남음(나중에 정한다): 공식 키를 만들어 `OFFICIAL_KEYS`와 비밀 값 `EXECUTORS_SIGNING_KEY`에 두기, 다시 서명을 자동으로 할지 |
| P6 | ~~Linux 격리(jailer 등)~~: sbx의 일이 아니다. VMM을 어떻게 가두는지는 실행기·어댑터와 VMM이 정한다. sbx는 실행기 규약으로 통신만 하고, 어떤 VMM이나 그 묶음 내용에도 기대지 않는다 | 뺌 |
| P7 | 자원 상한: MCP의 샌드박스 수·vCPU·메모리 합계, detached 샌드박스의 수명·유휴 시간, pull·prepare 디스크 사용량, egress 전송량, scratch 디스크 합계 (31의 MCP 부분) | 일부: `sbx mcp --max-sandboxes/--max-cpus/--max-memory` (기본 8개·4 vCPU·4 GiB, 시작 중인 것도 센다). 수명·유휴, 디스크, egress는 남음 |
| P8 | 취약점 처리 절차(SECURITY.md): 첫 응답 48시간, 판단 7일, 고침이 준비되면 엠바고 최대 14일·신고 뒤 90일 안 공개, GHSA로 CVE, "위협 모델 표의 경계를 넘으면 CVE", 지원 버전(최신 minor 둘, 각 1년 이상), 어댑터·커널·wick·upstream의 신고처, 릴리스 노트의 Security 절, 받은 것을 확인하는 법 | 완료 |
| P9 | 호환성 정책(`COMPATIBILITY.md`): semver, 지원 중단은 minor 하나 이상 경고한 뒤, 계약 목록(CLI 플래그·종료 코드, `--format json`은 더하기만, MCP 도구, 실행기 규약, 저장소 색인, data/cache 배치, `.sbx-executor`·라벨). 실행기 규약을 "같아야 한다"에서 범위 협상으로: sbx는 `MIN_VERSION..=VERSION`, 실행기는 `capabilities`의 `min_version`..`version`, 겹치는 가장 높은 버전으로 `Launch`. `capabilities`는 더하기만(모르는 필드 무시), 색인의 `min_protocol`. 남음: 디스크 배치 버전 파일(`layout-version`)과 옮기기/거부는 디스크 배치를 처음 바꿀 때 | 일부 |
| P10 | 거버넌스: CODE_OF_CONDUCT(Contributor Covenant 2.1), MAINTAINERS, GOVERNANCE, DCO(CI `dco` 작업이 PR 커밋을 본다), PR 양식. 남음: 브랜치·태그 rulesets(저장소 설정), 릴리스 권한을 가진 두 번째 메인테이너(지금은 한 명, 구한다고 적었다) | 일부 |
| P11 | OpenSSF Scorecard 작업(`scorecard.yml`, main push와 손으로, 결과 공개와 코드 스캔)과 README 배지. 남음: Best Practices 배지(passing, bestpractices.dev에 사람이 등록), OSPS Baseline 자가 증명 | 일부 |
| P12 | 색인의 `yanked`·`advisory`(저장소 규약), `executor ls`와 tui가 설치된 것 중 그렇게 표시된 버전을 알린다. 커널·VMM 패치 정책은 그 묶음을 만드는 쪽의 일이다 | 남음 |
| P13 | 운영: 감사 기록(누가 언제 무엇을 만들고 실행했나, 이미지 digest, 실행기 버전, allow 목록, exec argv·종료 코드, egress 허용·거부. JSONL, 보존 기간 설정, 선택적으로 OTel), `sbx stats`, `sbx system df`·`prune`, 고아 VM 정리 | 남음 |
| P14 | ~~호스트 강화 점검(SMT, KSM, KVM 권한 등)~~: VMM을 돌리는 호스트 설정은 실행기·VMM 쪽의 일이다 | 뺌 |
| P15 | SDK(Python, TypeScript): CLI JSON 계약을 감싸고 `cli-output.schema.json`으로 타입을 만든다(데몬 없음) | 남음 |
| P16 | 이름 바꾸기(`cowl` 등): Docker Sandboxes도 `sbx` 바이너리를 내고 USPTO에 DOCKER, INC.의 `SBX`(9류)가 있어 검토했다 | 뺌(2026-11-11): 이름은 sandbox, 명령은 `sbx`로 둔다 |
| P17 | 적대적 검토 1차(2026-11-11, 저장소·설치·MCP·install.sh·서명 도구): 고친 것은 대소문자만 다른 이름으로 출처 고정 피하기(이름·버전은 소문자), 앞 저장소가 실패하면 뒤 저장소로 이름이 넘어감(거부, `--repo`), 어댑터가 같은 이름의 다른 저장소로 넘어감, 이미지가 sbx 라벨(owner=mcp, 실행기 고정)을 줌, MCP 동시 create의 상한 경합, 원격 색인이 `file:`·loopback을 가리킴, redirect 중간 단계의 https→http, 색인 파싱 2차 시간(버전당 압축 파일 64개), install.sh에서 서명만 지우면 auto가 통과함·옛 서명본으로 바꿔치기(`SBX_VERSION`이면 그 태그), seen·목록 파일의 동시 쓰기(잠금), 설치의 검사–rename 경합(잠금), 받기 전 출처 검사, 저장소 이름 `local` 예약, Windows 장치 이름, `repo-sign`의 기한·serial 넘침, 키를 넣은 뒤 서명 없이 올리기(CI가 실패). 각각 회귀 테스트(잠금은 빼면 실패하는 동시성 테스트), install.sh는 가짜 릴리스·cosign으로 18가지 | 완료 |
| P18 | 적대적 검토 2차(2026-11-11): 어댑터 이름도 첫 저장소의 것(이미 설치한 어댑터도 그 출처여야 함), VMM의 출처를 어댑터보다 먼저 봄, 클라이언트가 이름 붙일 수 없는 색인 항목은 색인 전체가 아니라 그 항목만 건너뜀, 끝이 `.`인 이름(Windows에서 `fake.`=`fake`), `repo-add`·executors.yml도 같은 이름 규칙, 본 serial 파일은 적을 때만 잠금(읽기 전용 데이터 디렉터리), MCP 한도 설명을 실제와 맞춤(서버 사이 동시 생성은 마지막 자리를 함께 얻을 수 있음), 실행기 `capabilities`에 30초·2MiB 상한(멈춘 실행기가 설치 잠금을 쥐지 못함), e2e가 실패 이유를 정확한 문구로 확인. 각각 회귀 테스트(읽기 전용 seen 테스트는 고치기 전 코드에서 실패함을 확인했다. MCP 동시 호출 e2e는 지금 코드에 await가 없어 예약 없이도 통과하며, 앞으로 끼어들 await를 지킨다) | 완료 |

## 그 뒤

- 다른 외부 실행기 (libkrun 등). 규약(`sbx-executor-proto`)만 지키면 된다.
- Kubernetes·containerd(shim v2, RuntimeClass), 스냅샷과 빠른 시작(복제본마다 난수·boot_id를 새로), 다중 테넌트 서버 배포
- 언어 바인딩
- OCI 범위 확대 (`docs/plan/architecture.md` 3장의 "나중")
