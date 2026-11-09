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
| 5a | `rmi` 후 다른 이미지가 쓰지 않는 레이어 blob 삭제 | 동시에 같은 레이어를 쓰는 이미지를 pull해도 blob이 사라지지 않는다 | 완료 (규칙: `sandbox-store` `write::claim_blobs`, `write::collect_blobs`) |

## M2. 실행 경로 (Linux x64)

| 단계 | 내용 | 끝났다는 기준 | 상태 |
|---|---|---|---|
| 6 | 호스트-게스트 메시지 정의 | 프레이밍 위 인코딩, 버전 확인 | 완료 |
| 7 | 게스트 init: rootfs와 scratch를 overlay로 합침, exec, 파일 송수신 | 게스트 단독 테스트 | 완료: 연결 처리, scratch 포맷(단독 테스트, e2fsck, 커널 마운트), tty. 부팅 순서는 구현했고 실제 부팅은 외부 실행기 E2E에서 확인 |
| 8 | 게스트 init initramfs 만들기 (`xtask`), x86_64·aarch64 | `/init`이 정적 `sandbox-guest`인 initramfs | 완료 (`cargo xtask guest-initramfs`, 두 아키텍처 CI에서 만든다. GNU cpio·bsdcpio로 확인). 배포 빌드(`cargo xtask dist`)는 sbx 안에 내장하고 처음 쓸 때 캐시로 꺼낸다 |
| 9 | 실행기 통신 규약(`executor-proto`)과 외부 실행기 어댑터(`executor-process`) | 규약을 지키는 `sbx-fake-executor`로 적합성 테스트 통과 | 완료 |
| 10 | 실행 코어: create, start, exec, cp, stop, rm (attached) | fake 실행기로 통과 (실제 VM은 외부 실행기로 E2E) | 완료 (fake, macOS·Linux) |
| 11 | CLI `run`, `create`, `start`, `exec`, `cp`, `stop`, `rm`, `ps`, `inspect` (뒤에 `restart`, `kill`, `wait`) | `sbx run alpine echo hi`가 `hi`를 출력 | fake 실행기로 끝-끝 완료 (`sandbox-cli/tests/e2e.rs`: 출력, 종료 코드 125/126/127, `-i`, 시그널 전달, cp, Docker식 stop(StopSignal → `-t` 뒤 끄기), kill, wait, restart). 실제 이미지·VM은 외부 실행기 E2E |

VMM은 이 저장소 밖(Firecracker, 또는 같은 인터페이스를 구현한 fc-fork)의 것이다. 이 저장소에는 실행기 규약과,
규약을 Firecracker 공개 인터페이스로 잇는 adapter(`executor-firecracker`)가 있다.

**성공 기준 1 충족 (2026-11-09, macOS arm64 HVF, fc-fork + `executor-firecracker` + erofs·vsock을 켠 6.1 커널)**:
`sbx run --rm alpine:3.20 echo hi`가 실제 microVM에서 `hi`를 출력했다(처음 pull·prepare 포함 3.0초, 이후 `run --rm ... true` 0.55초).
같은 환경에서 확인: 종료 코드(7, 127), `-i`, `--cpus`·`-m`, overlay 루트, `run -d`·`ps`·`exec`·`cp`(양방향)·`logs`,
`stop`(SIGTERM을 받아 0으로 끝남, 0.16초), 네트워크 차단(`lo`만), `--network allow`(허용 호스트로 apk·curl 200, 그 밖은 차단).
알려진 차이: busybox `wget`은 HTTPS를 프록시에 `GET https://…`(절대 형식)로 보내 400을 받는다(`CONNECT`를 쓰는 curl, apk는 된다).
Linux KVM(upstream Firecracker)과 CI 자동 실행은 남았다.

## M3. 격리와 수명

| 단계 | 내용 | 상태 |
|---|---|---|
| 12 | detached 수명과 `sandbox-shim`, 고아 프로세스 정리 | 완료 (수명은 시작할 때 정한다: `run -d`, `start`/`start -a`. `logs`. `run -dit`와 `attach`(shim의 붙기 소켓, Ctrl+P Ctrl+Q로 떨어짐, Unix). shim이 죽으면 실행기 규약으로 VM이 끝나고 `stopped`. fake로 끝-끝 테스트) |
| 13 | 폴더 공유 copy, bind (bind는 실행기 capability가 있을 때) | 완료 (bind는 virtio-fs 태그로 실행기에 넘기고 게스트가 마운트. fake는 심볼릭 링크로 흉내. 실제 virtio-fs는 외부 실행기 E2E) |
| 14 | 네트워크 정책 장치 (기본 차단, 허용 방식) | 완료 (`--network allow --allow HOST:PORT`, 게스트 안 HTTP 프록시 + 호스트 허용 목록, 이름이 내부 주소로 풀리면 거부. fake와 curl로 끝-끝 테스트) |
| 15 | 정책 위반 시나리오 통합 테스트 (외부 접속, 공유 범위 밖 접근) | 정책 단위(허용 목록 거부, 내부 주소 거부, 복사 경로 탈출)는 테스트됨. 장치 없음·읽기 전용 공유의 실제 강제는 VM에서만 확인되므로 외부 실행기 E2E |

## M4. 공개 준비

| 단계 | 내용 | 상태 |
|---|---|---|
| 16 | 실행기 저장소 규약과 `sbx executor install NAME[@VERSION]`, `search`, `repo` | 완료 (`executor-repo`: 정적 호스팅 가능한 `index.json` + `.tar.gz` 묶음, SHA-256 확인. file:// 저장소에서 실제 firecracker 묶음 설치 0.18초) |
| 17 | 공식 저장소와 릴리스(CI) | 완료(첫 릴리스 전): 공식 저장소는 묶음을 GitHub Releases에, `index.json`을 GitHub Pages(gh-pages 브랜치의 `executors/`)에 둔다. sbx에 `official`로 기본 등록(지우면 남는다). `.github/workflows/release.yml`: 태그 `v<버전>`에서 sbx(Linux musl 정적, macOS arm64), 실행기 묶음(Linux는 upstream Firecracker로 만들고 KVM smoke 통과 뒤, macOS는 저장소 변수 `FC_FORK_TAG`가 있을 때 toollyst/firecracker 릴리스로), `repo-add --url`로 색인. toollyst/firecracker `v1.18.0-fork.1` 릴리스(macOS 15+, hypervisor 권한 서명)를 `FC_FORK_TAG`로 지정, 그 자산으로 macOS 단계를 재현해 실제 VM smoke 17/17(2026-11-10). 남음: Pages 소스를 gh-pages로 설정, 첫 태그 |
| 18 | 커널 | 완료: 보통 이미지는 실행기 묶음의 기본 리눅스 커널(아키텍처당 하나)로, 라벨 `io.github.toollyst.sandbox.kernel`로 커널을 알린 이미지는 그 커널로(실행기 규약 2의 `custom_kernel`). 실제 VM으로 확인. 기본 커널은 `cargo xtask kernel`(Linux 6.1.155, upstream Firecracker CI 설정 + `xtask/kernel/sbx.config`)로 CI에서 빌드하고, 이 커널로 macOS HVF smoke 17/17 |
| 19 | README, 데모, 설치 한 줄, 벤치마크 | 벤치마크 완료(macOS arm64 HVF: `run --rm alpine true` p50 121ms, `exec` p50 8ms, 10개 동시 0.50초). 설치 한 줄 완료(`install.sh`: `curl -fsSL https://toollyst.github.io/sandbox/install.sh \| sh`, SHA-256 확인, `~/.local/bin`, 기본 실행기까지. 로컬 file:// 릴리스로 확인). README 영어 완료. 데모 완료(`cargo xtask demo`: 실제 명령을 실제 시각대로 담은 움직이는 SVG `assets/demo.svg`, 바깥 도구 없이 CSS만. 머신이 한가할 때 다시 담는다) |
| 20 | Linux KVM(upstream Firecracker) 끝-끝과 CI | x86_64 완료: CI `vm` 작업이 GitHub 러너(KVM)에서 upstream Firecracker v1.17.0 + 빌드한 커널로 smoke 17/17과 bench. 남음: aarch64(GitHub arm 러너에 /dev/kvm이 없어 건너뛴다. 묶음은 만든다) |
| 21 | AI 에이전트 입구: MCP 서버 | 완료: `sbx mcp`(stdio, JSON-RPC 직접 구현). 도구 `run`, `create`, `exec`, `write_file`, `read_file`, `list`, `remove`. 정책 없음: 인자를 CLI 인자(`--이름=값`)로 바꿔 같은 파서로 해석, 네트워크는 `allow`로만, 호스트 마운트는 내보내지 않는다. 시간 제한(기본 300초). 이미지를 받는 동안 `notifications/progress`. e2e(가짜 실행기)와 macOS 실제 VM으로 확인 |
| 22 | Docker 사용 편의 | 완료: `--name`(ID 대신 어디서나, `cp 이름:경로`도), `ps -q`·`images -q`, `ps`의 CREATED·NAMES와 새것부터, `--env-file`, `logs --tail`, `sbx completion`(bash, zsh, fish, powershell, elvish), `sbx info`(호스트, 가상화, 실행기, 디렉터리), 받기 진행 막대(터미널일 때만). e2e로 확인 |

## 그 뒤

- 다른 외부 실행기 (libkrun 등). 규약(`executor-proto`)만 지키면 된다.
- 언어 바인딩
- OCI 범위 확대 (`docs/plan/architecture.md` 3장의 "나중")
