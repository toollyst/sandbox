# 개발 흐름 기획: build, 볼륨, 샌드박스 잇기

2026년 11월 13일 · 대상 릴리스 `v0.3.0`(compose는 그 뒤) · 상위 문서: [architecture.md](architecture.md), [로드맵](../roadmap.md)

이 문서는 방향과 범위만 다룬다. 구현하면 규칙의 정본은 코드(문서 주석, 타입, 테스트)로 옮기고, 이 문서는 방향이
바뀔 때만 고친다.

## 1. 왜

"믿지 않는 코드를 돌리는 샌드박스"만으로는 이야기가 약하다. E2B, Docker Sandboxes처럼 같은 자리를 노리는 도구가
많고, 개발자가 매일 여는 도구가 되지 못한다. 그래서 Docker Desktop의 **핵심**을 가져온다. 이미지를 만들고, 돌리고,
개발용 서비스(postgres, redis)를 띄워 앱과 잇는 것이다. 전부 컨테이너마다 VM이고, 빠르고, 기본으로 닫혀 있다.

한 줄로 말하면 이렇다. **build도 run도 VM 안에서.** 빌드도 믿지 않는 코드를 돌리는 일이다. `npm install`의
postinstall이나 `pip install`의 빌드 스크립트가 그렇다. Docker는 빌드를 모든 컨테이너가 함께 쓰는 커널(Linux에서는
호스트 커널, Docker Desktop에서는 그 VM 하나의 커널) 위에서 돌리고 네트워크를 기본으로 열어 둔다. sbx는 빌드마다
따로 VM을 띄우고, 나가는 연결은 허용 목록으로만 연다.

## 2. 목표와 하지 않는 것

목표:

- `sbx build .`가 Dockerfile로 OCI 이미지를 만든다. 결과는 `sbx run`, `docker run`, 레지스트리 어디서나 쓴다.
- `sbx push`로 레지스트리에 올린다.
- 이름 붙은 볼륨으로 DB 데이터가 샌드박스를 지워도 남는다.
- 한 샌드박스가 다른 샌드박스의 정해진 포트에 닿는다(앱에서 `db:5432`로).
- 소스 폴더를 고치면 샌드박스 안에 바로 보인다(`-v ./src:/app`, virtio-fs가 있는 실행기에서).
- 위 기능 위에 `compose.yaml`의 일부를 읽는 `sbx compose`.

하지 않는 것: GUI(있는 것은 `sbx tui`), Kubernetes, 확장, Docker Engine API 소켓, Compose 전체, 아키텍처 에뮬레이션
(architecture.md 3장, 그래서 빌드도 호스트 아키텍처만), Swarm, `docker network`의 브리지·오버레이 네트워크.

지키는 것:

- **실행기 규약은 바꾸지 않는다.** build, 볼륨, 잇기는 이미 있는 `Launch`(디스크 목록, vsock)로 된다. 그래서 규약을
  지키는 어느 실행기에서나 된다. 실행기에 기대는 것은 실시간 마운트(virtio-fs) 하나뿐이다. sbx 쪽은 이미 되어 있고
  (`live_share`), sbx 릴리스는 실행기가 그것을 갖출 때를 기다리지 않는다(7.3).
- **정책은 호출자가 정한다.** 빌드의 네트워크도 `run`처럼 기본 차단이다. 열지 말지는 사용자가 `--allow`로 정한다.
- **조용히 바꾸지 않는다.** 지원하지 않는 Dockerfile 명령, 다른 아키텍처, 없는 기능은 오류로 알린다.

참고한 것: BuildKit(단계 캐시 키, `--output type=…`, `--mount=type=cache`), buildah(데몬 없는 빌드, OCI layout으로
쓰기), kaniko(컨테이너 안에서 rootfs 스냅샷으로 층 만들기), Apple `container`(컨테이너마다 VM, macOS 전용), Docker
Compose 명세, podman volume.

### 2.1 깔면 바로 돈다 (2026-11-13)

지금은 설치한 뒤 `sbx executor install wick`을 따로 해야 하고, 하지 않으면 첫 `sbx run`이 실행기가 없다는 오류로 끝난다.
이 한 단계가 처음 쓰는 사람을 놓친다. 그래서 이렇게 바꾼다.

- 설치 스크립트(`install.sh`, `install.ps1`)가 `sbx`를 깐 뒤 기본 실행기를 깐다. 끄는 길을 둔다(`SBX_EXECUTOR=none`).
- 실행기가 하나도 없을 때 VM이 필요한 명령(`run`, `create`, `start`, `build`)은 공식 저장소에서 기본 실행기를 받아 깔고
  이어서 한다. 한 줄로 무엇을 하는지 알린다(`installing the default executor wick@… from official`). 끄는 길을 둔다
  (`SBX_NO_AUTO_EXECUTOR=1`). 터미널이 아니어도 묻지 않고 한다: 서명을 확인한 공식 저장소의 것만 받기 때문이다.
- 받는 길은 지금의 `sbx executor install`과 같다. 색인의 minisign 서명(공개키는 `sbx` 안), `serial`·`expires`, 묶음의
  sha256. 다른 저장소가 앞에 있어도 자동 설치는 공식 저장소의 이름만 본다.
- **묶이는 곳은 설치 스크립트다.** 버전마다 고정한 `install.sh`·`install.ps1`이 "이 sbx 버전과 이 wick 버전(과 그
  어댑터 버전)"의 짝을 담는다. 스크립트는 그 둘만 깐다(묶음의 sha256도 그 안에). `sbx` 바이너리와 그 빌드는 wick을
  모른다. 그래서 sbx의 빌드와 릴리스 자산은 지금처럼 wick에 묶이지 않고, 짝은 내보낼 때(publish) 정한다.
- 릴리스마다 그 버전의 스크립트를 릴리스 자산과 `…/sandbox/v<버전>/install.sh`로 올린다. 버전 없는 주소
  (`…/sandbox/install.sh`)는 가장 새 릴리스의 스크립트를 가리킨다. 지금의 `SBX_VERSION`은 버전 있는 스크립트를 받아
  돌리는 것으로 바꾼다. 같은 스크립트는 언제 돌려도 같은 sbx와 같은 VMM을 깐다.
- 짝은 저장소의 파일 하나(`default-executor.toml`, 플랫폼마다 `wick@<버전>`과 `firecracker@<버전>`)에 적고, publish가
  그것으로 스크립트를 만든다. publish는 그 짝을 실제로 깔아 smoke를 돌린 뒤 올린다.
- **스크립트를 거치지 않은 경우**(`sbx upgrade`, 소스 빌드, 패키지 관리자)에도 같은 짝을 쓰도록, publish가 같은 값을
  서명한 릴리스 색인(`releases.json`의 그 버전 항목)에도 적는다(제안). 실행기가 없을 때의 자동 설치와 `sbx upgrade`는
  색인에서 자기 버전의 짝을 읽어 그 버전을 받는다. 색인에 닿지 못하거나 짝이 없으면 공식 저장소의 가장 새 것을 받고
  그렇게 했다고 알린다.
- **설치 스크립트의 버전은 sbx 버전이다.** 스크립트는 sbx 버전을 따라가고, 한 번 올린 버전의 스크립트와 그 짝은 바꾸지
  않는다. 짝을 바꿔야 하면(wick의 보안 고침 등) sbx의 패치 버전을 새로 내고 그 버전의 스크립트에 새 짝을 담는다.
- `sbx upgrade`로 sbx가 바뀌면 짝도 바뀐다. 기본 실행기가 자동으로 깔린 것이면(사용자가 `executor use`로 고르지
  않았으면) 새 짝의 버전을 받아 기본으로 삼는다. 사용자가 고른 것은 건드리지 않는다. 그래서 "자동으로 깔았다"는 표시를
  설치 기록에 둔다. 이미 만든 샌드박스는 만들 때의 실행기에 묶여 있으므로(라벨) 그대로 돈다.
- 실행기를 하나라도 손으로 깔았거나 `--executor`를 줬으면 자동 설치는 하지 않는다. 바꾸는 것은 지금처럼
  `sbx executor install NAME[@VERSION]`, `sbx executor use NAME`이다.
- PR의 CI는 지금처럼 로컬 저장소와 upstream Firecracker로 돌고 공식 저장소를 보지 않는다. 공식 저장소
  (toollyst/sandbox-executors)의 첫 버전은 2026-11-14(토)쯤 올라온다. 2026-11-13에는 아무것도 없다(404).
- 저장소에 닿지 못하면 지금의 오류에 까닭과 손으로 하는 길(`sbx executor install`, `--from DIR`)을 보인다.

## 3. 정한 것 (2026-11-13)

| 질문 | 결정 | 버린 길과 까닭 |
|---|---|---|
| 실행기 | 규약과 추상화는 그대로 둔다. 다만 **깔면 바로 돈다**: wick이 기본 실행기이고, 설치 스크립트와 첫 실행이 서명된 공식 저장소에서 자동으로 받는다(2.1). 다른 실행기로 바꿀 수 있다 | wick에 직접 기대기: 열린 규약을 잃는다. 릴리스 묶음에 wick 넣기: sbx 릴리스가 wick 빌드에 묶이고 묶음이 20 MB 넘게 커진다. 지금처럼 따로 고르게 하기: 깔고 나서 한 단계가 더 있으면 쓰지 않는다 |
| 빌더 | **직접 만든다.** Dockerfile을 읽고 스테이지마다 VM 하나를 띄워 `RUN`을 거기서 돌린다(4장) | BuildKit을 VM 안에서: Dockerfile 호환은 완벽하지만 게스트에 컨테이너 런타임(runc, cgroup, namespace)과 buildkitd 이미지가 필요하고, VM 안에 컨테이너를 또 띄우므로 빠르다는 이야기가 약해진다 |
| 실시간 마운트 | **실행기의 virtio-fs**(wick에 넣는다). sbx 쪽에 동기화를 따로 만들지 않는다(7.3) | sbx 쪽 한 방향 sync: 양방향이 아니어서 `-v`와 다르게 동작하고, 감시와 지우기 처리를 sbx가 떠안는다 |
| 릴리스 범위 | **0.3에 개발 흐름 전부**: build, push, 볼륨, `--connect`. compose는 그 뒤 | 0.3은 build와 push만: 빨리 내지만 "postgres까지 이어 쓰는" 데모가 안 된다 |
| 빌드 결과와 저장 | **고른 대상의 형태 하나만 만든다**(4.1). 저장은 층 단위로, 같은 층은 한 번만(4.7) | 한 번 빌드해 OCI blob과 EROFS를 함께 두기: 디스크를 두 번 쓰고, 바탕 이미지가 이미지마다 되풀이된다 |
| 홍보 문구 | 0.3에서 README의 첫 줄과 비교표를 바꾼다 | 지금 바꾸기: 아직 없는 기능을 말하게 된다 |

## 4. `sbx build`

### 4.1 명령

Docker와 buildx의 이름을 그대로 쓴다.

```sh
sbx build -t app .                                   # 기본: sbx 저장소에 넣고 rootfs까지 준비, 곧바로 sbx run app
sbx build -t app -f docker/Dockerfile ./src          # Dockerfile과 컨텍스트를 따로
sbx build -t app --build-arg NODE_ENV=production .
sbx build -t app --target test .                     # 이 스테이지까지만
sbx build -t app --no-cache .
sbx build -t app --allow registry.npmjs.org:443 .    # RUN이 나갈 곳(run과 같은 규칙)
sbx build -t app --output type=docker .              # docker 데몬에 load
sbx build -t app --output type=oci,dest=app.tar .    # OCI 아카이브(podman, skopeo)
sbx build -t ghcr.io/me/app:1 --push .               # 레지스트리로
```

- **고른 대상의 형태 하나만 만든다.** 두 형태를 함께 만들어 두지 않는다. 디스크를 두 번 쓰지 않기 위해서다.
  `--output`을 여러 번 주면 그만큼만 만든다.
- `type=sbx`(기본): sbx에서 돌릴 형태만 둔다. 층마다 EROFS 하나이고 이미지끼리 같은 층은 한 번만 저장한다(4.7).
  압축한 OCI 층 blob은 만들지 않는다.
- `type=docker`: OCI 아카이브를 흘려 `docker load`에 넘긴다. sbx 저장소에는 이미지를 두지 않는다. `docker`가 없으면 오류다.
- `type=oci,dest=…`: 파일이면 tar, 디렉터리면 OCI image layout. 마찬가지로 sbx 저장소에는 두지 않는다.
- `type=sbx`로 만든 이미지를 나중에 `push`·`save`하면 그때 층을 OCI tar로 바꿔 내보낸다. 층의 digest는 처음 내보낼 때
  정하고 기록해, 다시 내보내도 같다.
- `--push`(`type=registry`): 6장의 push.
- Firecracker에서 sbx 없이 바로 부팅되는 rootfs는 내지 않는다. 지금 EROFS 디스크는 sbx 게스트 init과 디스크
  구성에 묶인 내부 형식이고, 내보내면 공개 형식이 된다. 형식이 굳으면 그때 따로 정한다.
- `--platform`은 호스트 아키텍처만 받는다. 다른 아키텍처는 에뮬레이션이 필요하므로 오류다.

### 4.2 빌드 한 번의 흐름

스테이지 하나는 VM 하나다. 단계마다 VM을 새로 띄우지 않는다.

1. **해석.** Dockerfile을 읽어 스테이지와 단계의 목록으로 바꾼다. `ARG`와 `--build-arg`를 풀고, `--target`에
   필요 없는 스테이지는 뺀다. 지원하지 않는 명령은 이 단계에서 줄 번호와 함께 오류다.
2. **캐시 찾기.** 단계마다 캐시 키를 만든다(4.4). 앞에서부터 맞는 단계를 찾고, 처음으로 맞지 않는 단계에서 멈춘다.
3. **바탕 준비.** `FROM` 이미지를 지금처럼 pull하고, 캐시에 맞은 층까지 얹어 rootfs 하나로 `prepare`한다
   (`sbx-rootfs`가 메모리에서 합친다). 모든 단계가 맞으면 VM을 띄우지 않고 끝난다. 이 `prepare`는 바탕 이미지
   전체를 다시 합치므로 큰 이미지(node, python)에서는 초 단위가 걸린다. 그래서 바탕 이미지의 EROFS는 지금처럼 캐시에서
   그대로 쓰고, 캐시에 맞은 층은 빌드 VM의 scratch에 층 tar로 넣어 위층으로 푸는 쪽을 먼저 잰다(D3에서 정한다).
4. **빌드 VM.** 그 rootfs로 샌드박스를 하나 띄운다. 사용자의 `--allow`, `--cpus`, `-m`을 따른다. 빌드용이라는 라벨
   (`io.github.toollyst.sandbox.build`)을 붙이고, 빌드가 끝나면 지운다.
5. **단계 실행.** 게스트가 단계마다 scratch 위에 새 위층을 만들어 overlay를 쌓는다(4.3).
   - `RUN`: 그 층에서 실행한다. env, workdir, user, `SHELL`은 앞 단계까지의 상태를 따른다. 0이 아닌 종료 코드면 빌드가
     실패하고 그 출력을 보여 준다. 허용 목록에 없어 거절된 이름이 있었으면 함께 알린다(8.5).
   - `COPY`, `ADD`: 호스트가 컨텍스트에서 골라(`.dockerignore` 적용) 그 층에 넣는다. 지금의 `CopyIn`이다.
     `--from=스테이지`는 그 스테이지의 층을 호스트에서 메모리로 합쳐 원하는 경로만 꺼낸다. VM이 필요 없다.
   - `ENV`, `WORKDIR`, `USER`, `LABEL`, `EXPOSE`, `ENTRYPOINT`, `CMD` 같은 것은 이미지 config만 바꾸고 층을 만들지 않는다.
6. **층 내보내기.** 단계가 끝나면 게스트가 그 위층을 항목의 열로 보내고 호스트가 OCI 층 tar로 쓴다(4.3). overlay의 whiteout(문자 장치 0/0,
   opaque xattr)은 OCI의 `.wh.` 파일로 바꾼다. 호스트는 압축하고 diff_id와 digest를 계산해 blob으로 저장한다.
7. **조립.** config와 manifest를 쓰고 4.1의 출력으로 보낸다. 독립된 스테이지들은 동시에 빌드한다.

받은 층 tar는 게스트가 만든 것이므로 믿지 않는 입력이다. 지금 `CopyOut`과 레이어 병합이 지키는 규칙(경로 탈출 거부,
크기 상한, scratch 크기 `--storage-opt size=`)을 그대로 적용한다.

### 4.3 게스트 쪽: 빌드 층

부팅은 바꾸지 않는다(실행기 규약과 커널 명령줄을 건드리지 않는다). 게스트 init은 지금처럼 rootfs와 scratch의 overlay
안으로 chroot한다. 호스트가 층을 열라고 하면(`OpenLayer`) 게스트가 그 처음 뿌리로 돌아가 앞 층을 내리고, 새 overlay
(lower = rootfs와 앞 층들, upper = scratch의 새 디렉터리)를 마운트해 그 안으로 chroot한다. 그래서 `Exec`와 `CopyIn`은
고치지 않아도 새 층 위에서 돈다. 보통의 `run`은 새 코드를 지나지 않는다.

- 층을 내보내라고 하면(`ExportLayer`) 게스트가 위층을 항목의 열로 보낸다(whiteout과 opaque는 종류로). tar는 호스트가
  쓴다. 게스트가 보낸 것은 믿지 않는 입력이므로 호스트에 tar 파서를 새로 두지 않고, digest와 시각을 한 곳에서 정한다.
- 내보내도 층은 열린 채로 남는다. 다음 `OpenLayer`가 앞 층을 내린다. 앞 단계의 프로세스가 층을 쥐고 있으면 오류다.
  그래서 빌더는 단계가 끝나면 남은 프로세스를 끝낸다(D3).
- 층은 500개까지다(overlayfs의 상한).
- 게스트 규약에는 메시지를 더하기만 하고 버전은 올리지 않는다. `sbx`를 올리기 전에 띄운 VM은 새 메시지만 모르고
  `exec`, `cp`, `stop`은 그대로 된다.
- 규칙의 정본: `crates/app/guest/src/overlay.rs`, `layer.rs`, `crates/core/runtime/src/layer.rs`.

### 4.4 캐시

- 키: 앞 단계의 키, 명령 문자열(`ARG`를 푼 뒤), 그 단계가 쓰는 build arg, `COPY`·`ADD`면 고른 파일들의 내용과 메타데이터 해시.
  `RUN`의 결과가 네트워크에 따라 달라지는 것은 Docker처럼 키에 넣지 않는다(`--no-cache`가 있다).
- 키 → 층 digest의 기록은 `cache_dir/build/`에 둔다. 쓰는 것은 관리 쪽이다(architecture.md 1장). 층 blob은 이미
  있는 blob 저장소와 정리 규칙(`claim_blobs`, `collect_blobs`)을 쓴다. `sbx system prune`이 빌드 캐시도 치운다.
- `SOURCE_DATE_EPOCH`를 주면 층 tar의 항목 순서와 시각을 고정한다(BuildKit의 `rewrite-timestamp`와 같다). "확인할 수
  있다"는 목표와 맞다. 다만 `RUN`이 매번 다른 내용(로그, 받은 패키지의 새 버전)을 쓰면 digest는 달라진다. 보장하는 것은
  "sbx가 만드는 차이는 없다"까지다.

### 4.5 Dockerfile 범위

| 범위 | 명령과 기능 |
|---|---|
| 처음 | `FROM`(`AS`, `scratch`, `FROM` 앞 `ARG`), `RUN`(셸·exec 형식), `COPY`(`--from`, `--chown`, `--chmod`), `ADD`(로컬 파일, 로컬 tar 풀기), `ENV`, `ARG`, `WORKDIR`, `USER`, `LABEL`, `EXPOSE`, `VOLUME`, `ENTRYPOINT`, `CMD`, `STOPSIGNAL`, `SHELL`, 파서 지시어 `escape`, `.dockerignore`, `-f -` |
| 나중 | `RUN --mount=type=cache`(볼륨을 그대로 쓴다), `RUN --mount=type=secret`, heredoc, `ADD <URL>`(호스트가 받으므로 같은 허용 목록), `ONBUILD`, `COPY --link`, git 컨텍스트 |
| 하지 않음 | `# syntax=`로 다른 프런트엔드 고르기(무시하지 않고 알린다), `HEALTHCHECK`(config에 쓰지만 sbx는 적용하지 않는다고 알린다) |

### 4.6 크레이트

- Dockerfile 해석은 adapter(`sbx-dockerfile`)에 둔다. OCI 자료형이 `sbx-oci` 밖으로 나가지 않는 것처럼, Dockerfile
  자료형도 그 크레이트 밖으로 나가지 않는다. 밖으로는 `sbx-model`의 빌드 계획(스테이지와 단계)만 나간다.
- 빌드 코어(`sbx-build`)는 계획, 캐시 키, 층과 config 조립을 맡는다. VM을 띄우는 일과 저장소에 쓰는 일은 trait 뒤에
  두고, CLI가 `sbx-runtime`과 `sbx-manage`로 채운다. `run`이 관리와 실행을 차례로 부르는 것과 같은 모양이다. 그래서
  실행과 관리가 서로 모른다는 규칙은 그대로다. 정확한 규칙은 `RULES`에 더한다.

### 4.7 저장: 층 하나는 한 번만

지금은 이미지마다 압축한 층 blob(`save`용)과 층을 모두 합친 `rootfs.erofs`를 함께 둔다. 같은 바탕을 쓰는 이미지가
다섯이면 바탕이 다섯 번 들어가고, 빌드가 들어오면 빌드마다 전체 크기의 디스크가 하나씩 는다. Docker보다 디스크를 더
쓰게 되므로 0.3에서 고친다.

- 저장 단위를 층으로 바꾼다. 층마다 EROFS 하나(`layers/<diff_id>/layer.erofs`), 이미지는 층의 목록이다. 같은 층은
  이미지와 빌드 캐시가 함께 쓴다. 빌드 캐시가 따로 차지하는 공간은 없다.
- VM에는 층들을 읽기 전용으로 붙이고 게스트가 overlay의 lower로 쌓는다. 빌드 모드의 층 쌓기(4.3)와 같은 방식이다.
  실행기의 `max_disks`보다 층이 많으면 아래 층들을 하나로 합친 EROFS를 만들어 붙인다(합친 것은 캐시이고 지울 수 있다).
- 받은(pull) 이미지의 압축 blob은 층 EROFS를 만든 뒤 지운다. `save`·`push`는 층을 다시 tar로 바꾼다. 받은 그대로의
  digest가 필요하면(`save`가 같은 이미지 ID를 내는 지금 동작) 압축 전 tar의 digest(diff_id)는 그대로이므로 이미지 ID는
  같고, 압축 blob의 digest만 달라질 수 있다. 이것은 Docker 호환 표(`compat.rs`)에 적는다. `--keep-blobs`로 남길 수 있다.
- 배치 버전(`layout-version`)을 올리고, 옛 `rootfs.erofs`는 다음 `prepare` 때 층으로 바꾼다.
- `sbx system df`가 층을 몇 이미지가 함께 쓰는지 보여 준다.

## 5. 출력과 도구

- 진행은 `pull`처럼 사람에게는 단계마다 한 줄씩 보여 준다(`[2/7] RUN npm ci`, 캐시면 `CACHED`). `--progress plain`이면
  RUN 출력을 그대로 흘린다. `--format json`이면 단계 이벤트를 JSON Lines로 낸다(`output.rs`의 공개 자료형).
- `sbx events`에 이미지 이벤트 `build`를 더한다.
- `sbx tui`: 이미지 목록에서 빌드한 이미지와 그 Dockerfile 경로를 보이고, `:` 없이 키 하나로 다시 빌드한다
  (같은 `sbx build` 명령을 자식으로 부른다).
- MCP 도구로 `build`를 여는 것은 나중에 따로 정한다. 에이전트가 빌드까지 VM 안에서 하는 것은 잘 맞지만, 컨텍스트로
  보낼 호스트 디렉터리를 어디까지 허락할지 먼저 정해야 한다.

## 6. `sbx push`

- `sbx push NAME[:TAG]`와 `sbx build --push`. OCI Distribution의 push: blob이 이미 있는지 묻고(`HEAD`), 없으면 올리고
  (monolithic, 크면 chunked), 같은 레지스트리 안에서는 cross-repo mount를 먼저 시도하고, 마지막에 manifest를 올린다.
- 인증은 지금 읽는 파일(Docker `config.json`, credential helper, containers `auth.json`)에 push scope로 요청한다.
- `registries.conf`의 미러는 push에 쓰지 않는다. 이름이 가리키는 레지스트리로만 올린다. `blocked`면 거부한다.
- pull한 이미지는 받은 문서 그대로(`save`와 같다)이므로 다시 올려도 digest가 같다.
- architecture.md 3장의 Distribution "나중"에서 push를 "지금"으로 옮긴다.

## 7. 개발용 서비스: 볼륨, 잇기, 실시간 마운트

### 7.1 이름 붙은 볼륨

```sh
sbx volume create pgdata
sbx run -d --name db -v pgdata:/var/lib/postgresql/data -p 5432:5432 postgres:17
sbx volume ls / inspect / rm / prune
```

- 볼륨 하나는 `data_dir/volumes/<이름>/`의 sparse 디스크 이미지 하나다. VM에 디스크로 하나 더 붙이고(실행기의
  `max_disks` 안에서), 게스트가 처음 쓸 때 scratch처럼 포맷한 뒤 대상 경로에 마운트한다.
- 실행기 규약의 디스크 역할(`DiskRole`, 지금은 `rootfs`·`scratch`)에 `volume`을 더하고 규약 버전을 올린다.
  `DiskSpec`은 모르는 값을 거부하므로 옛 실행기에는 볼륨을 보내지 않는다.
- 볼륨의 flush는 호스트 디스크까지 가야 한다(DB 데이터). Firecracker 어댑터는 `volume` 디스크를
  `cache_type: "Writeback"`으로 붙인다. 지금 어댑터는 `cache_type`을 주지 않아 기본값 `Unsafe`이고, 그러면 게스트에
  flush 기능이 보이지 않는다. wick(macOS)에서 게스트 `fdatasync` 한 번이 호스트 `F_FULLFSYNC`까지 가는 것을 쟀다
  (4–5 ms, 2026-11-13 wick 답변). `Writeback`은 upstream Firecracker에도 있다.
- 비어 있는 볼륨을 처음 붙이면 Docker처럼 이미지의 그 경로 내용을 먼저 채운다.
- 쓰기로 붙인 볼륨은 한 번에 한 샌드박스만 쓴다(블록 장치는 두 커널이 함께 쓸 수 없다). 두 번째는 오류이고
  Docker 호환 표(`compat.rs`)에 적는다. 읽기 전용(`:ro`)은 쓰는 쪽이 없을 때 여럿이 붙일 수 있다.
- MCP 서버는 호스트 폴더를 붙이지 않는 것처럼, 사용자가 만든 볼륨도 붙이지 않는다. MCP로 만든 볼륨만 쓴다.
- `-v NAME:/path`와 `-v ./dir:/path`는 Docker 규칙(첫 글자가 `/`, `.`, `~`이면 경로)으로 가른다. 지금은 이름이면
  오류를 낸다.

### 7.2 샌드박스 잇기

```sh
sbx run -d --name db postgres:17
sbx run --rm --connect db:5432 app        # 앱 안에서 db:5432로 닿는다. 다른 곳은 여전히 닫혀 있다
```

- `--connect NAME:PORT`는 허용 목록과 같은 생각이다. 무엇에 닿을지 하나씩 연다.
- 8장의 투명 TCP를 그대로 쓴다. 게스트가 `NAME`을 물으면 호스트가 `--connect` 목록에서 찾아 가짜 주소를 주고,
  게스트는 그 주소의 `PORT`에서 듣는다. 받은 연결은 vsock으로 호스트에 가고, 호스트는 대상 샌드박스의 게스트에
  지금의 `Forward`로 넘긴다. VM에는 여전히 네트워크 장치가 없다.
- HTTP 프록시가 아니라 TCP를 그대로 넘기므로 postgres, redis 같은 어느 프로토콜이든 된다.
- 연결마다 대상을 새로 찾는다. 그래서 대상이 다시 시작해도 다음 연결은 된다. 대상이 없으면 그 연결만 끊긴다.
  기록은 `events/network`에 `connect`로 남는다.
- 같은 `data_dir`을 쓰는 샌드박스끼리만 된다.
- MCP의 샌드박스는 MCP로 만든 샌드박스에만 `--connect`할 수 있다. 지금 MCP 도구가 MCP로 만든 샌드박스만 보는 것과 같은
  선이다. 그러지 않으면 에이전트가 사용자의 DB에 닿는다.

### 7.3 실시간 마운트

- `-v ./src:/app[:ro]`는 sbx 쪽이 이미 되어 있다. 실행기가 `live_share`를 알리면 virtio-fs 태그로 넘기고 게스트가
  마운트한다(로드맵 13). 알리지 않으면 지금처럼 오류이고, 조용히 복사로 바꾸지 않는다.
- 남은 것은 실행기의 일이다. upstream Firecracker에는 virtio-fs가 없으므로 wick에 넣는다(wick 저장소의 일).
  virtio-fs가 있는 다른 실행기(libkrun 등, 로드맵 "그 뒤")도 그대로 된다.
- sbx 0.3은 이것을 기다리지 않는다. wick이 갖추면 `sbx executor install`로 받은 새 wick에서 바로 된다.
- 0.3에서 sbx가 하는 것: `live_share`가 없을 때의 오류에 "이 실행기는 실시간 공유가 없다, `--mount type=copy`나
  virtio-fs가 있는 실행기를 쓰라"를 넣고, smoke에 실행기가 `live_share`를 알릴 때만 도는 실시간 공유 확인을 더한다.

wick 답변(2026-11-13)에서 sbx 쪽 일로 정해진 것:

- **기능 알아내기.** wick은 config 최상위 `extensions`(`fs`, 나중에 `gpu`)로 새 장치를 받는다. upstream Firecracker는
  모르는 키를 조용히 무시하므로, 어댑터는 `firecracker --features`(wick 전용, JSON)가 성공하고 그 확장을 알릴 때만
  `extensions`를 넣고 `live_share: true`를 알린다. 실패하면 upstream으로 보고 아무것도 넣지 않는다.
- **호스트 변경 알림.** virtio-fs는 호스트 쪽 변경을 게스트 inotify로 전하지 않는다. 그러면 핫 리로드(vite, webpack,
  nodemon)가 돌지 않는다. sbx가 호스트에서 공유 폴더를 감시하고(FSEvents, inotify, ReadDirectoryChangesW) vsock으로
  게스트에 알리면, 게스트가 거기서 알림을 만든다. 바뀐 파일은 게스트에서 시각을 그대로 다시 써서 `IN_ATTRIB`을
  만들 수 있지만, 생기고 지워진 파일은 그렇게 흉내 낼 수 없다. 어떤 감시 도구가 무엇을 보는지(libuv, chokidar,
  watchdog, notify) 재고 방법을 정한다. wick은 내용이 곧바로 맞게 보이도록 캐시 무효화를 맡는다.
- **소유자와 권한.** sbx가 바라는 동작: 호스트의 파일은 언제나 호스트 사용자 소유, 게스트에서는 접근하는 uid가 주인처럼
  보이고, 게스트 안 `chown`은 성공해 xattr로 남는다. Docker Desktop의 동작과 같다. 정하는 것은 wick이다.
- 게스트 커널에 `CONFIG_VIRTIO_FS`를 켠다(어댑터 묶음의 커널, `cargo xtask kernel`).
- virtio-fs가 붙은 VM은 스냅샷을 거부한다(wick).

### 7.4 시계

개발용 DB처럼 며칠씩 도는 샌드박스는 시계가 맞아야 한다(TLS, 토큰, 로그). wick 답변으로는 호스트가 잠자는 동안 게스트
시계가 멈추고 깬 뒤 자동으로 보정하지 않는다. 잠자기가 없어도 하루 약 0.35초 늦어진다.

- 게스트 init(PID 1, sbx의 것)이 맞춘다. 소유 프로세스(`sbx`나 `sbx-shim`)가 vsock으로 호스트의 시각을 주기적으로,
  그리고 벽시계가 단조 시계보다 크게 뛰었을 때(잠자기에서 깸) 곧바로 보낸다. 게스트는 차이가 크면 바로 맞추고,
  작으면 천천히 맞춘다(`clock_adjtime`).
- 이미지에 chrony가 없어도 되고, 실행기 기능(`ptp_kvm`)에도 기대지 않는다. 그래서 어느 이미지, 어느 실행기에서나 된다.
- `sbx-guest-proto`에 시각 메시지를 더한다.

## 8. 나가는 길: 프록시를 모르는 프로그램

### 8.1 문제

지금 `--network allow`의 길은 게스트 안 HTTP 프록시 하나다. `HTTP_PROXY`를 따르는 프로그램(curl, apk, apt, pip, npm,
cargo)은 나가지만, 따르지 않는 프로그램은 허용 목록에 있어도 나가지 못한다. 빌드와 개발에서 흔히 걸리는 것은 이렇다.

- ssh로 받는 `git`(`git@github.com:…`, 포트 22)과 `git://`
- HTTP가 아닌 TCP 클라이언트: 바깥 postgres·redis·mongo, SMTP, gRPC 일부
- 프록시를 읽지 않는 다운로더(언어 런타임이나 설치 스크립트가 직접 소켓을 여는 것)
- 그리고 무엇이 막혔는지 모른다. 이름이 게스트 안에서 바로 NXDOMAIN으로 끝나므로 호스트에는 아무 기록도 남지 않는다.

### 8.2 방법: 가짜 주소 DNS와 투명 TCP

VM에 네트워크 장치를 붙이지 않고, 이름 단위 허용 목록과 호스트의 강제를 그대로 두고, TCP라면 어느 프로그램이든 나가게 한다.

1. **이름을 호스트에 묻는다.** 게스트의 루프백 DNS(지금은 늘 NXDOMAIN)가 A 질의를 받으면 호스트에 `Lookup{name}`을
   보낸다(빌려 준 egress 연결 하나로).
2. **호스트가 목록으로만 답한다.** 이름이 허용 목록(`--allow`)이나 `--connect`에 맞으면 열린 포트들을 돌려준다. 맞지
   않으면 거절하고 `events/network`에 `refuse`(이름)를 남긴다. 이때 호스트는 그 이름을 실제로 풀지 않는다. 그래서
   허용하지 않은 이름은 DNS로도 새어 나가지 않는다.
3. **게스트가 가짜 주소를 준다.** 허용된 이름에 `198.18.0.0/15`(RFC 2544, 실제로 쓰이지 않는 범위)에서 주소 하나를 주고,
   그 주소의 허용된 포트마다 듣는 소켓을 연다. 그 뒤에 DNS 응답을 보낸다. 범위 전체는 부팅 때 루프백의 local 경로로
   잡는다(`ip route add local 198.18.0.0/15 dev lo`와 같은 netlink 메시지 하나).
4. **연결을 넘긴다.** 프로그램이 가짜 주소로 연결하면 게스트가 받아 주소를 이름으로 되돌리고, 지금 프록시가 보내는 것과
   같은 `Connect{host, port}`를 보낸다. 호스트 쪽 확인(이름 풀기, 사설 주소 거부, `--egress-limit`, 기록)은 한 줄도
   바뀌지 않는다.

성질:

- 프로그램은 목적지를 속일 수 없다. 고를 수 있는 것은 가짜 주소뿐이고, 그 주소는 허용된 이름 하나에 묶여 있다. 호스트는
  그 이름으로 연결한다.
- 게스트가 허용 목록을 알 필요가 없다. 정본은 여전히 호스트의 목록이고, 망가진 게스트가 할 수 있는 일은 지금과 같다.
- IP로 적은 규칙(`--allow 140.82.112.3:22`)은 그 주소를 루프백의 `/32` local 주소로 더하고 같은 방법으로 듣는다.
- AAAA 질의에는 "이름은 있고 주소는 없다"(NOERROR, 빈 답)로 답해 프로그램이 IPv4를 쓰게 한다. NXDOMAIN으로 답하면
  일부 리졸버는 이름이 없다고 본다.
- `HTTP_PROXY`는 지금처럼 함께 둔다. 프록시를 따르는 프로그램은 그대로 프록시로 나간다. 두 길 모두 같은 목록을 지난다.
- 게스트 커널에 더 바라는 것이 없다. IPv4 루프백, local 경로, 보통의 소켓뿐이다. 커널은 실행기의 것이므로 netfilter나
  TUN 같은 선택 기능에 기대지 않는다.

### 8.3 남는 한계

- **포트가 `*`인 규칙.** 어느 포트로 올지 미리 모르면 들을 수 없다. `HOST:*`는 지금처럼 프록시로만 나간다. 투명 TCP에는
  포트를 적어야 한다고 문서와 경고에 쓴다.
- **UDP.** DNS 말고는 여전히 나가지 못한다. QUIC(HTTP/3)은 보통 TCP로 돌아간다.
- **이미지의 `/etc/resolv.conf`.** 이미지가 바깥 이름 서버(`8.8.8.8` 등)를 적어 두면 질의가 루프백 DNS에 오지 않는다.
  그래서 `--network allow`나 `--connect`가 있을 때는 Docker처럼 sbx가 `/etc/resolv.conf`를 `nameserver 127.0.0.1`로
  덮는다(overlay 위층에, 이미지는 그대로). 지금 `dns.rs`의 "건드리지 않는다"를 바꾸는 결정이다.
- **자체 DNS를 쓰는 프로그램**(DoH, 이름 서버를 직접 고르는 것)은 나가지 못한다. 의도한 대로다.
- **비공개 저장소의 ssh 키.** 나가는 길과 별개로 키가 VM 안에 있어야 한다. 빌드에서는 `RUN --mount=type=ssh`(나중)로
  호스트의 ssh agent를 그 단계에만 빌려 준다.

### 8.4 버린 길

| 길 | 버린 까닭 |
|---|---|
| VM에 네트워크 장치를 붙이고 호스트의 사용자 공간 스택(passt, gVisor netstack)으로 거른다 | "네트워크 장치가 없다"는 경계를 버린다. 실행기 기능이 더 필요하고, 거르는 단위가 이름이 아니라 IP가 된다 |
| 게스트 안 TUN과 사용자 공간 TCP 스택(smoltcp 등) | 모든 포트와 UDP까지 되지만 게스트 init에 TCP 스택이 들어간다(`sbx-guest`는 `libc`만 쓴다). 실행기 커널에 TUN이 있어야 한다. `HOST:*`가 꼭 필요해지면 다시 본다 |
| 게스트 netfilter(REDIRECT, TPROXY) | 실행기 커널의 선택 기능에 기댄다. nftables netlink를 게스트에 들여야 한다 |
| `LD_PRELOAD`로 `connect()` 가로채기 | 정적 바이너리(Go 등)는 지나간다 |

### 8.5 범위

빌드만의 기능이 아니다. `run`, `exec`, MCP 샌드박스 모두 `--network allow`면 이 길을 얻는다. `sbx-guest-proto`에
`Transparent`, `Lookup`, `Resolved`를 더했다. 실행기 규약은 바뀌지 않는다. 규칙의 정본은
`crates/app/guest/src/transparent.rs`, `dns.rs`, `crates/core/runtime/src/egress.rs`다.

거절된 **이름**은 호스트에 기록되므로 `sbx build`가 실패할 때 "막힌 이름: `example.com`, `--allow example.com:PORT`를
더하라"를 보여 줄 수 있다. 포트는 보여 줄 수 없다. DNS 질의에는 포트가 없고, 허용된 이름의 열지 않은 포트로 가는 연결은
게스트 안에서 바로 끊겨 호스트가 알지 못한다. 그때는 그 이름에 열린 포트를 함께 알린다.

## 9. `sbx compose` (마지막)

- `compose.yaml`의 일부만 읽는다: `services`의 `image`, `build`, `command`, `entrypoint`, `environment`, `env_file`,
  `ports`, `volumes`(이름 붙은 볼륨과 경로), `depends_on`(시작 순서만), 최상위 `volumes`.
- 서비스마다 위의 명령으로 옮긴다. Compose는 기본 네트워크 안에서 서로 다 닿으므로, 서비스마다 다른 서비스의
  `expose`·`ports`·이미지 `ExposedPorts`로 `--connect`를 만든다. 무엇을 열었는지 `sbx compose config`가 보여 준다.
- 모르는 키는 오류다. 무시하면 사용자는 된 줄 안다.
- `up`, `down`, `ps`, `logs`, `build`. 상태는 샌드박스 라벨(`io.github.toollyst.sandbox.compose.project`)에만 둔다.
  따로 저장하지 않는다.

## 10. 단계

모든 단계는 CLI와 `sbx tui`를 함께 한다. "끝났다는 기준"은 fake 실행기 e2e와 CI `vm` 작업의 smoke 둘 다다.

| 단계 | 내용 | 끝났다는 기준 |
|---|---|---|
| X1 | Docker 호환 표: 코드의 표가 정본, 문서와 진행률은 테스트가 만든다(로드맵 M9 X1) | Docker CLI 참조 문서의 모든 명령과 `run`·`build`·`exec`·`cp`·`logs`·`ps`의 모든 옵션에 줄이 있다. 줄과 clap이 어긋나면 테스트가 실패한다 |
| D1 | `sbx-dockerfile`: 해석과 4.5의 처음 범위, 빌드 계획 자료형 | Docker 문서의 예제와 공개 저장소 Dockerfile 묶음을 읽고, 지원하지 않는 것은 줄 번호로 거부한다. 퍼징 대상 |
| D2 | 게스트 빌드 모드: 층 쌓기, 층 내보내기(whiteout 바꾸기), 규약 버전 올리기 | 게스트 단독 테스트와 fake로, 파일 추가·바꾸기·지우기·디렉터리 비우기가 OCI 층으로 맞게 나온다 |
| S1 | 층 단위 저장(4.7): 층 EROFS, 이미지는 층 목록, 게스트가 층을 쌓아 부팅, 배치 버전 올리기 | 같은 바탕의 이미지 둘이 바탕 층을 한 번만 차지한다(`system df`). 옛 캐시가 옮겨진다. `run` p50이 지금보다 느려지지 않는다 |
| D3 | `sbx-build`와 `sbx build`: 스테이지당 VM 하나, COPY·RUN, 캐시, `type=sbx` | 다단계 Dockerfile(node, python, go 예제)이 빌드되고 `sbx run`으로 돈다. 두 번째 빌드는 VM 없이 끝난다 |
| D4 | 출력 `type=docker`, `type=oci`, `SOURCE_DATE_EPOCH` | `docker run`으로 같은 결과. `COPY`와 결정적인 `RUN`만 있는 Dockerfile을 두 번 빌드하면 digest가 같다 |
| D5 | `sbx push`, `build --push` | 로컬 레지스트리(`registry:2`)와 GHCR에 올리고 다른 도구로 받는다 |
| D6 | bench: 같은 Dockerfile을 `docker build`와 견준다(처음, 캐시, 한 줄 바꿈) | README에 숫자를 싣는다 |
| N1 | 투명 TCP(8장): `Lookup`, 가짜 주소 DNS, 게스트 리스너, `resolv.conf` 덮기, 막힌 이름 기록 | ssh로 `git clone`(`--allow github.com:22`)과 바깥 postgres에 `psql`이 되고, 목록에 없는 이름은 호스트가 풀지 않은 채 `refuse`로 남는다. `HOST:*`는 프록시로만 나간다 |
| V1 | 볼륨 | postgres 데이터가 `rm` 뒤에도 남고, 두 번째 쓰기 붙이기는 거부된다 |
| V2 | `--connect` | 앱 샌드박스에서 `psql -h db`가 되고, 열지 않은 포트와 다른 샌드박스는 닿지 않는다 |
| V3 | 실시간 마운트(7.3): 어댑터의 `--features` 확인과 `extensions.fs`, 호스트 변경 알림, 안내와 smoke | `live_share`가 없는 실행기에서 오류가 다른 길을 알리고, upstream에는 `extensions`를 넣지 않는다. wick에서 vite 개발 서버가 호스트에서 고친 파일에 다시 읽는다 |
| V4 | 시계 맞추기(7.4) | 호스트를 재운 뒤 깨워도 게스트 시계가 1초 안에 맞고, 하루 돌려도 어긋남이 0.1초 아래다 |
| R1 | 문서, README 문구와 비교표, `build` → `run` → `--connect`로 postgres까지 잇는 데모 → 0.3.0 | docs-before-main 확인 |
| C1 | `sbx compose`(0.3 뒤) | 앱 + postgres + redis 예제가 `up` 한 번으로 뜨고 `down`으로 정리된다 |

## 11. 위험

- **Dockerfile 호환.** 사용자는 "내 Dockerfile이 그냥 돈다"를 기대한다. 처음 범위로 흔한 Dockerfile 대부분은 되지만
  `--mount=type=cache`를 쓰는 Dockerfile이 늘고 있다. 공개 저장소의 Dockerfile 묶음으로 얼마나 되는지 재고, 그
  숫자로 "나중" 순서를 정한다.
- **빌드 네트워크 기본 차단.** 처음 쓰는 사람은 막힌 연결부터 만난다. 막힌 `host:port`와 그대로 붙여 쓸 수 있는
  `--allow`를 오류에 넣는 것이 이 기능의 첫인상이다(8.5). 투명 TCP 뒤에도 남는 것(`HOST:*`, UDP)은 흔한 Dockerfile
  묶음으로 얼마나 걸리는지 D3에서 재고 문서에 적는다.
- **이름.** Docker Sandboxes도 `sbx`다. Docker의 영역으로 들어갈수록 헷갈림이 커진다. README의 "Not Docker
  Sandboxes" 절을 위로 올리는 것을 0.3에서 함께 본다.
- **범위가 커진다.** Docker Desktop 대체로 보이면 Compose 전체, 브리지 네트워크, Engine API 요청이 온다. 2장의
  "하지 않는 것"을 README에도 적는다.
