# 프로젝트 구조와 OCI 적용 기획

2026년 11월 9일 · 상위 문서: sandbox 기획서

이 문서는 방향과 범위만 다룬다. 세부 규칙의 정본은 코드다. 규칙은 각 크레이트의 문서 주석에 적고, 타입과 테스트로 강제한다. 결정이 바뀌면 코드와 테스트를 바꾸고, 이 문서는 방향이 바뀔 때만 고친다.

| 규칙 | 정본 위치 |
|---|---|
| 크레이트 사이 의존 규칙 | `xtask/src/main.rs`의 `RULES`, `FORBIDDEN_FEATURES` (`cargo xtask check-deps`) |
| 각 크레이트의 책임과 알면 안 되는 것 | 각 크레이트 `lib.rs`의 `//!` 문서 |
| 상태와 전이 | `sandbox-model`의 `State::can_transition_to` |
| 이미지 config와 호출자 값을 합치는 규칙 (Docker `run`) | `sandbox-runtime/src/process.rs` |
| 실행기 기능 검사 (bind 미지원 시 오류 등) | `sandbox-runtime/src/validate.rs` |
| 캐시 레이아웃, 잠금, 사용 중 보호, blob 정리 | `sandbox-store` |
| 레이어 병합 규칙 (whiteout, opaque, 하드 링크) | `sandbox-rootfs/src/lib.rs` |
| 이미지 참조 해석 | `sandbox-oci/src/reference.rs` |
| OCI 적용 범위 (받는 형식, 적용하는 필드) | `sandbox-oci/src/scope.rs` |
| CLI 명령 목록 | `sandbox-cli/src/commands.rs` (`sbx help`가 그대로 출력) |
| CLI 동작 (`--pull`, `rmi` 보호, Docker와 다른 점) | `sandbox-cli/src/behavior.rs` |
| 호스트-게스트 메시지 (연결 단위, 순서, 인코딩, 신뢰 경계) | `sandbox-proto` |

## 1. 원칙

- 샌드박스 1개 = VM 1개 = 이미지 1개.
- 샌드박스는 호출되거나, 그 위에서 무언가가 실행되는 대상이다. 세션을 어떻게 쓸지, 무엇을 허용할지는 호출자가 정한다. 샌드박스는 그것을 수행하는 기본 동작과 강제 장치만 제공한다.
- **모든 동작은 명시적이다.** 어떻게 동작하는지 정해져 있고, 문서(코드의 문서 주석)대로 동작한다는 뜻이다.
- **CLI는 Docker CLI를 기준으로 한다.** 다른 점은 코드에 목록으로 둔다.
- **관리와 실행을 분리한다.**
  - 관리(`sandbox-manage`): 이미지 받기, rootfs 준비, 삭제. cache_dir에 쓰는 것은 관리뿐이다.
  - 실행(`sandbox-runtime`): 샌드박스 생명주기. 캐시는 읽기만 하고 data_dir만 쓴다.
  - Docker처럼 `run`이 이미지를 받아오는 동작은 CLI가 관리와 실행을 차례로 불러서 만든다.
- 수명은 attached(호출 프로세스와 함께 끝남)와 detached(남음)를 모두 지원한다. detached는 데몬 없이 샌드박스마다 `sandbox-shim`을 하나씩 띄운다.
- data_dir과 cache_dir은 설정으로 받는다. 같은 경로를 주면 공유되고, 다른 경로를 주면 따로 쓴다.
- 폴더 공유는 bind(실시간, 실행기가 지원할 때만)와 copy(복사 후 명시적 회수)를 둘 다 두고 호출자가 고른다.
- rootfs 형식은 인터페이스 하나 뒤에 여러 개를 붙인다. 하나씩 구현하며 첫 번째는 erofs다. 빌더는 Rust로 직접 구현한다.
- 구현 언어는 Rust다. 지금 노출하는 것은 CLI `sbx`뿐이고 언어 바인딩은 나중에 붙인다. 코어와 CLI는 별도 크레이트다.

## 2. 구조

```text
crates/
  sandbox-model         공용 자료형
  sandbox-proto         호스트-게스트 메시지 틀
  sandbox-executor      실행기 trait          ← executor-fake, executor-firecracker
  sandbox-image         이미지 소스 trait     ← sandbox-oci
  sandbox-rootfs        레이어 병합 + rootfs writer trait (형식별 구현은 feature)
  sandbox-store         cache_dir 레이아웃, 잠금 (쓰기는 feature "write")
  sandbox-runtime       실행 코어
  sandbox-manage        관리 코어
  sandbox-guest         게스트 init (바이너리)
  sandbox-shim          detached 감시 프로세스 (바이너리)
  sandbox-cli           sbx (바이너리)
xtask/                  check-deps 등 개발용 작업
docs/                   기획과 로드맵만
```

의존 방향은 세 가지로 요약된다.
- 코어(runtime, manage)는 trait 크레이트에만 의존한다.
- 구현체를 고르는 것은 바이너리뿐이다.
- runtime과 manage는 서로 모른다.

정확한 규칙은 `xtask`의 `RULES`가 정본이고 CI에서 검사한다.

## 3. OCI 적용 범위

OCI는 입력 형식이다. OCI 자료형은 `sandbox-oci` 밖으로 나가지 않는다. 범위를 넓힐 때는 `sandbox-oci`(와 필요하면 `sandbox-model`의 필드)만 고친다.

| 스펙 | 지금 | 나중 | 하지 않음 |
|---|---|---|---|
| Distribution | pull, 익명과 Bearer 토큰 인증, OCI와 Docker v2 manifest, 모든 blob의 digest 검증 | push, 자격 증명 헬퍼, 미러, 서명 검증, 오프라인 가져오기 | Docker schema1 |
| Image | linux와 호스트 아키텍처 선택, tar와 tar+gzip 레이어, whiteout, config의 `Entrypoint`/`Cmd`/`Env`/`WorkingDir`/`User` | zstd, `StopSignal`, `Labels`, `ExposedPorts`·`Volumes`(정보로만 표시) | 아키텍처 에뮬레이션, `Healthcheck` 등 Docker 확장 |
| Runtime | 입력으로 받지 않는다. 상태 이름과 용어만 맞춘다 | 번들 입력(별도 변환 크레이트) | namespace, seccomp 등 VM 경계에서 의미 없는 필드는 적용하지 않음 (목록은 `scope.rs`) |

## 4. 검증

| 대상 | 방법 | 환경 |
|---|---|---|
| 의존 규칙 | `cargo xtask check-deps` | 모든 OS |
| 코어 규칙 (상태, 병합, 참조, CLI 동작, 잠금) | 단위 테스트 | 모든 OS |
| 실행 코어 | `executor-fake` | 모든 OS |
| 실행기 적합성 | 같은 테스트를 fake와 실제 어댑터에 돌림 | 실제: Linux x64 |
| 통합 | 공개 이미지 pull → prepare → run → exec → cp | Linux x64 |
| KVM 사용 가능 여부 | `kvm-probe` 작업이 기록만 함 | Linux x64, arm64 |

## 5. 아직 정하지 않은 것

- 레이어를 병합할 때 파일 내용을 다시 읽을지 임시 저장할지 (`sandbox-rootfs`의 `ContentSource` 뒤에 숨겨 두었다)
- 네트워크 허용 방식과 그 CLI 표현 (`NetworkPolicy`에는 지금 `Disabled`만 있다)
- 게스트 기반 이미지(커널, 게스트 init)의 배포 방식
