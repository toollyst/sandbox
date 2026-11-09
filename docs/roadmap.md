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
| 5a | `rmi` 후 다른 이미지가 쓰지 않는 레이어 blob 삭제 | 동시에 같은 레이어를 쓰는 이미지를 pull해도 blob이 사라지지 않는다 | 남음 (잠금 설계 필요) |

## M2. 실행 경로 (Linux x64)

| 단계 | 내용 | 끝났다는 기준 |
|---|---|---|
| 6 | 호스트-게스트 메시지 정의 | 프레이밍 위 인코딩, 버전 확인 |
| 7 | 게스트 init: rootfs와 scratch를 overlay로 합침, exec, 파일 송수신 | 게스트 단독 테스트 |
| 8 | 게스트 커널 빌드 (`xtask`), x86_64·aarch64 | erofs, overlay, vsock, virtio-blk를 켠 커널 |
| 9 | Firecracker 어댑터 | 적합성 테스트를 fake와 함께 통과 |
| 10 | 실행 코어: create, start, exec, cp, stop, rm (attached) | Linux CI에서 실제 VM으로 통과 |
| 11 | CLI `run`, `create`, `start`, `exec`, `cp`, `stop`, `rm`, `ps`, `inspect` | `sbx run alpine echo hi`가 `hi`를 출력 |

M2가 끝나면 성공 기준 1을 충족한다.

## M3. 격리와 수명

| 단계 | 내용 |
|---|---|
| 12 | detached 수명과 `sandbox-shim`, 고아 프로세스 정리 |
| 13 | 폴더 공유 copy, bind (bind는 실행기 capability가 있을 때) |
| 14 | 네트워크 정책 장치 (기본 차단, 허용 방식) |
| 15 | 정책 위반 시나리오 통합 테스트 (외부 접속, 공유 범위 밖 접근) |

## 그 뒤

- 실행기 어댑터 추가 (fc-fork, libkrun)
- 언어 바인딩
- OCI 범위 확대 (`docs/plan/architecture.md` 3장의 "나중")
