# 프로젝트 구조와 OCI 적용 기획

2026년 11월 10일 · 상위 문서: sandbox 기획서

이 문서는 방향과 범위만 다룬다. 세부 규칙의 정본은 코드다. 규칙은 각 크레이트의 문서 주석에 적고, 타입과 테스트로 강제한다. 결정이 바뀌면 코드와 테스트를 바꾸고, 이 문서는 방향이 바뀔 때만 고친다.

| 규칙 | 정본 위치 |
|---|---|
| 크레이트 사이 의존 규칙 | `xtask/src/main.rs`의 `RULES`, `FORBIDDEN_FEATURES` (`cargo xtask check-deps`) |
| 크레이트 안 모듈 의존 방향 (순환 금지) | `xtask/src/modules.rs` (`cargo xtask check-deps`) |
| 실행기 통신 규약 (외부 실행기가 지킬 규격, 게스트에 대한 약속) | `sbx-executor-proto` |
| 각 크레이트의 책임과 알면 안 되는 것 | 각 크레이트 `lib.rs`의 `//!` 문서 |
| 상태와 전이 | `sbx-model`의 `State::can_transition_to` |
| 이미지 config와 호출자 값을 합치는 규칙 (Docker `run`) | `crates/core/runtime/src/process.rs` |
| 실행기 기능 검사 (bind 미지원 시 오류 등) | `crates/core/runtime/src/validate.rs` |
| 캐시 레이아웃, 잠금, 사용 중 보호, blob 정리 | `sbx-store` |
| 레이어 병합 규칙 (whiteout, opaque, 하드 링크) | `crates/port/rootfs/src/lib.rs` |
| 이미지 참조 해석 | `crates/adapter/oci/src/reference.rs` |
| OCI 적용 범위 (받는 형식, 적용하는 필드) | `crates/adapter/oci/src/scope.rs` |
| CLI 명령 목록 | `crates/app/cli/src/commands.rs` (`sbx help`가 그대로 출력) |
| CLI 동작 (`--pull`, `rmi` 보호, Docker와 다른 점) | `crates/app/cli/src/behavior.rs` |
| 호스트-게스트 메시지 (연결 단위, 순서, 인코딩, 신뢰 경계) | `sbx-guest-proto` |
| 실행기 저장소 (schema 1 `index.json`, schema 2 `v2/index.json`, 어댑터와 VMM 묶음) | `sbx-executor-repo` |
| 설치한 실행기·어댑터 배치, 버전 고르기 (주 버전과 패치, `@latest`) | `sbx-executor-install` |
| 어느 실행기를 쓸지 (`--executor`, 샌드박스 고정, `.sbx-executor`, 기본, PATH) | `crates/app/cli/src/choose.rs` |
| CLI 출력 계약 (`--format json`의 모양, `--jq`, JSON 오류) | `crates/app/cli/src/output.rs` (`docs/developers/cli-output.md`를 만든다) |
| `--filter` 키와 결합 규칙 | `crates/app/cli/src/filter.rs` |
| 이벤트 기록 장치 (덧붙이기, 세대, 따라 읽기) | `sbx-events`. 무엇을 기록하는지는 `sbx-runtime`, `sbx-manage` 문서 |
| 셸 자동완성 후보 | `crates/app/cli/src/complete.rs` |
| `sbx tui` 키와 동작 | `crates/app/cli/src/tui/keys.rs`(키 목록 정본), `crates/app/cli/src/tui/mod.rs` |

## 1. 원칙

- 샌드박스 1개 = VM 1개 = 이미지 1개.
- 샌드박스는 호출되거나, 그 위에서 무언가가 실행되는 대상이다. 세션을 어떻게 쓸지, 무엇을 허용할지는 호출자가 정한다. 샌드박스는 그것을 수행하는 기본 동작과 강제 장치만 제공한다.
- **모든 동작은 명시적이다.** 어떻게 동작하는지 정해져 있고, 문서(코드의 문서 주석)대로 동작한다는 뜻이다.
- **CLI는 Docker CLI를 기준으로 한다.** 다른 점은 코드에 목록으로 둔다.
- **관리와 실행을 분리한다.**
  - 관리(`sbx-manage`): 이미지 받기, rootfs 준비, 삭제. cache_dir에 쓰는 것은 관리뿐이다.
  - 실행(`sbx-runtime`): 샌드박스 생명주기. 캐시는 읽기만 하고 data_dir만 쓴다.
  - Docker처럼 `run`이 이미지를 받아오는 동작은 CLI가 관리와 실행을 차례로 불러서 만든다.
- 수명은 attached(호출 프로세스와 함께 끝남)와 detached(남음)를 모두 지원하고, Docker처럼 시작할 때 고른다. detached는 데몬 없이 샌드박스마다 `sbx-shim`을 하나씩 띄운다.
- 변화 알림(`sbx events`)도 데몬 없이, 상태를 바꾸는 프로세스가 바꾸는 자리에서 data_dir(샌드박스)과 cache_dir(이미지)에 직접 덧붙인다.
- CLI 출력이 계약이다. 사람(`sbx tui`), 스크립트, 에이전트가 같은 `--format json` 출력을 쓰고, 그 모양은 저장 형식과 떨어진 공개 자료형이다. TUI는 코어를 부르지 않고 같은 `sbx`를 자식으로 부른다.
- data_dir과 cache_dir은 설정으로 받는다. 같은 경로를 주면 공유되고, 다른 경로를 주면 따로 쓴다.
- 폴더 공유는 bind(실시간, 실행기가 지원할 때만)와 copy(복사 후 명시적 회수)를 둘 다 두고 호출자가 고른다.
- 네트워크는 기본 차단이다. VM에는 네트워크 장치를 붙이지 않는다. 허용은 호출자가 준 `HOST:PORT` 목록이고,
  게스트 안 HTTP 프록시가 받은 TCP 연결을 VM 소유 프로세스가 목록을 확인한 뒤 대신 연다(실행기 기능이 필요 없다).
  UDP와 프록시를 따르지 않는 프로그램은 나가지 못한다. 규칙은 `sbx-model`의 `AllowRule`, 강제는 `sbx-runtime`의 `egress`.
- rootfs 형식은 인터페이스 하나 뒤에 여러 개를 붙인다. 하나씩 구현하며 첫 번째는 erofs다. 빌더는 Rust로 직접 구현한다.
- 구현 언어는 Rust다. 지금 노출하는 것은 CLI `sbx`뿐이고 언어 바인딩은 나중에 붙인다. 코어와 CLI는 별도 크레이트다.

## 2. 구조

```text
crates/                      계층 폴더. 아래 계층만 쓴다(cargo xtask check-deps)
  model/                     sbx-model             공용 자료형
  spec/                      공개 규약. 다른 프로그램이 구현한다
    guest-proto/             sbx-guest-proto       호스트-게스트 메시지
    executor-proto/          sbx-executor-proto    실행기 통신 규약 (외부 실행기에 주는 규격)
    executor-install/        sbx-executor-install  설치된 실행기 배치
    executor-repo/           sbx-executor-repo     실행기 저장소 규약과 클라이언트
  port/                      코어가 기대는 인터페이스와 공용 부품
    executor/                sbx-executor          실행기 trait        ← sbx-executor-process, sbx-executor-fake
    image/                   sbx-image             이미지 소스 trait   ← sbx-oci
    rootfs/                  sbx-rootfs            레이어 병합 + rootfs writer trait (형식별 구현은 feature)
    events/                  sbx-events            이벤트 기록 장치 (JSON Lines, 잠금, 세대 둘)
    store/                   sbx-store             cache_dir 레이아웃, 잠금 (쓰기는 feature "write")
  adapter/                   port 구현
    oci/                     sbx-oci               OCI 레지스트리 이미지 소스
    executor-process/        sbx-executor-process  규약을 말하는 외부 실행 파일을 띄우는 어댑터
  core/
    runtime/                 sbx-runtime           실행 코어
    manage/                  sbx-manage            관리 코어
  app/                       구현을 고르고 조립하는 바이너리
    cli/                     sbx-cli               sbx
    shim/                    sbx-shim              detached 소유 프로세스 (라이브러리 + sbx-shim)
    guest/                   sbx-guest             게스트 init
  executor/                  실행기 (규약을 말하고 VM을 띄운다)
    firecracker/             sbx-executor-firecracker  Firecracker adapter (sbx-executor-firecracker)
    fake/                    sbx-executor-fake     테스트용 실행기 (같은 프로세스 구현 + sbx-executor-fake)
xtask/                       check-deps, deps-doc, dist, smoke 등 개발용 작업
docs/                        기획(plan/), 로드맵, 개발자 문서(developers/, 영어)
```

**책임 분담**: VMM은 이 저장소 밖(Firecracker, macOS에서는 그 빌드인 wick)의 책임이다. 이 저장소는 생명주기, 코어, CLI, 게스트 init,
실행기가 맞춰야 할 통신 규약(`sbx-executor-proto`), 실행기 저장소 규약(`sbx-executor-repo`), Firecracker adapter를 책임진다.
규약에 맞는 실행기는 그대로 실행한다. 실행기는 VMM을 자기 옆에 담은 자체 완결 묶음이거나, VMM만 담은 **VMM 묶음**이다.
VMM 묶음은 이름으로 가리킨 **어댑터**(규약을 말하는 프로그램, 예: `firecracker` 어댑터와 그 기본 커널)가 돌린다.
그래서 공식 VMM 묶음 `wick@<wick 릴리스 버전>`(태그 `v1.18.0+wick.1`이면 `wick@1.18.0+wick.1`)은 Firecracker 실행 파일뿐이고,
어댑터와 따로 올리고 고른다.
누구나 자기 VMM 빌드를 어댑터에 붙여 등록할 수 있다(`sbx executor install --from DIR --adapter NAME NAME@VERSION`).

**게스트 init 배포**: 게스트 init initramfs는 `sbx` 실행 파일 안에 들어 있다(`cargo xtask dist`, 빌드 대상 아키텍처의 것 하나).
사용자의 이미지는 바꾸지 않는다. VM을 켤 때 이미지 rootfs와 별도로 initramfs를 붙이고, 처음 쓸 때 캐시의
`guest/sha256-<내용 digest>/<arch>/initramfs`로 꺼낸다. `--guest-init`으로 다른 것을 줄 수 있다.

**커널**: 보통 이미지(Docker Hub의 alpine 등)에는 커널이 없다. 그래서 실행기(Firecracker는 어댑터 묶음)에 기본 리눅스 커널이 들어 있고
(아키텍처당 하나, 모든 이미지가 함께 쓴다) 보통은 그것으로 부팅한다. 이미지가 자기 커널을 담고 있으면
라벨 `io.github.toollyst.sandbox.kernel=<이미지 안 경로>`로 알린다. 그러면 `prepare`가 그 커널을 캐시의
`images/<hex>/kernel`로 꺼내고, 그 커널로 부팅한다(실행기가 `custom_kernel`을 알릴 때만. 아니면 띄우지 않는다).

의존 방향은 네 가지로 요약된다.
- 코어(runtime, manage)는 trait 크레이트에만 의존한다.
- 구현체를 고르는 것은 바이너리뿐이다.
- runtime과 manage는 서로 모른다.
- 크레이트 안에서도 위 모듈만 아래 모듈을 쓴다. 순환은 없다.

정확한 규칙은 `xtask`의 `RULES`가 정본이고 CI에서 검사한다.

## 3. OCI 적용 범위

OCI는 입력 형식이다. OCI 자료형은 `sbx-oci` 밖으로 나가지 않는다. 범위를 넓힐 때는 `sbx-oci`(와 필요하면 `sbx-model`의 필드)만 고친다.

| 스펙 | 지금 | 나중 | 하지 않음 |
|---|---|---|---|
| Distribution | pull, 익명과 Bearer 토큰 인증, OCI와 Docker v2 manifest, 모든 blob의 digest 검증 | push, 자격 증명 헬퍼, 미러, 서명 검증, 오프라인 가져오기 | Docker schema1 |
| Image | linux와 호스트 아키텍처 선택, tar, tar+gzip, tar+zstd 레이어(프레임 여럿, 건너뛰는 프레임 포함), whiteout, config의 `Entrypoint`/`Cmd`/`Env`/`WorkingDir`/`User`/`StopSignal`/`Labels`(호출자 `-l`이 이긴다) | `ExposedPorts`·`Volumes`(정보로만 표시) | 아키텍처 에뮬레이션, `Healthcheck` 등 Docker 확장 |
| Runtime | 입력으로 받지 않는다. 상태 이름과 용어만 맞춘다 | 번들 입력(별도 변환 크레이트) | namespace, seccomp 등 VM 경계에서 의미 없는 필드는 적용하지 않음 (목록은 `scope.rs`) |

## 4. 검증

| 대상 | 방법 | 환경 |
|---|---|---|
| 의존 규칙 | `cargo xtask check-deps` | 모든 OS |
| 코어 규칙 (상태, 병합, 참조, CLI 동작, 잠금) | 단위 테스트 | 모든 OS |
| 실행 코어 | `sbx-executor-fake` | 모든 OS |
| 실행기 적합성 | 같은 테스트를 fake와 실제 어댑터에 돌림 | 실제: Linux x64 |
| 통합 | 공개 이미지 pull → prepare → run → exec → cp | Linux x64 |
| 실제 VM | CI `vm` 작업: 로컬 v2 저장소에서 사용자처럼 설치한 뒤 `cargo xtask smoke`, `bench` (arm64는 러너에 KVM이 있을 때만) | Linux x64, arm64 |
| CLI 출력 계약 | 생성 문서(`cli-output.md`, JSON Schema)가 자료형과 같은지, e2e가 모든 JSON 출력을 자료형으로 엄격히 읽는다 | 모든 OS |

## 5. 아직 정하지 않은 것

- 레이어를 병합할 때 파일 내용을 다시 읽을지 임시 저장할지 (`sbx-rootfs`의 `ContentSource` 뒤에 숨겨 두었다)
