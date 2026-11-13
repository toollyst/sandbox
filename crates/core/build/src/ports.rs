//! 빌드 코어가 바깥에 바라는 것: 저장소([`Store`]), 빌드 VM([`Machines`], [`Vm`]), `.dockerignore`([`Ignore`]).
//!
//! 바이너리가 관리 코어(`sbx-manage`)와 실행 코어(`sbx-runtime`)로 채운다. 이 크레이트는 그 둘을 모른다.

use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;

use sbx_model::{BoxFuture, ImageId, Layer};
use sbx_rootfs::{ContentRef, LayerEntry};

use sbx_model::{Arch, ImageConfig};

use crate::error::BuildError;

/// 바탕 이미지 하나(`FROM 이미지`, `COPY --from=이미지`).
#[derive(Debug, Clone)]
pub struct Base {
    /// 이미지 ID.
    pub id: ImageId,
    /// 층. 아래부터.
    pub layers: Vec<Layer>,
    /// 이미지 config 문서(OCI image config JSON) 그대로. 없으면(0.2.1 전에 받은 이미지) `None`이고, 그때는 빌드가
    /// [`Base::fallback`]으로 시작한다.
    pub config: Option<Vec<u8>>,
    /// config 문서가 없을 때 쓸 실행 기본값(이미지 기록의 것).
    pub fallback: sbx_model::ImageConfig,
}

/// 캐시 기록 하나: 캐시 키 → 층.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheRecord {
    /// 그 단계가 만든 층.
    pub layer: Layer,
    /// 층을 만든 시각(RFC 3339). 이미지 history에 쓴다. 그래서 모두 캐시에서 온 빌드는 같은 이미지 ID다.
    pub created: String,
}

/// 층 파일 내용을 여는 쪽. [`Store::read_layer`]가 돌려준다.
pub trait Contents: Send + Sync {
    /// 내용을 연다.
    fn open(&self, content: &ContentRef) -> io::Result<Box<dyn Read + Send>>;
}

/// 저장소. 관리 코어가 채운다.
pub trait Store: Send + Sync {
    /// 이미지 참조를 찾는다. 없으면 받는다(`sbx run`과 같다).
    fn image<'a>(&'a self, reference: &'a str) -> BoxFuture<'a, Result<Base, BuildError>>;

    /// 캐시 기록. 없거나 그 층이 더는 없으면 `None`.
    fn cache_get(&self, key: &str) -> Result<Option<CacheRecord>, BuildError>;

    /// 캐시 기록을 쓴다. 같은 키는 덮는다.
    fn cache_put(&self, key: &str, record: &CacheRecord) -> Result<(), BuildError>;

    /// 압축하지 않은 층 tar(`tar`)를 층으로 저장한다. diff_id는 저장소가 직접 계산한다. 그 tar는 믿지 않는 입력이다
    /// (게스트가 보낸 것일 수 있다).
    fn add_layer(&self, tar: &Path) -> Result<Layer, BuildError>;

    /// 층 하나의 항목을 읽는다. `index`는 [`ContentRef::layer`]에 쓸 번호다. 내용은 돌려준 [`Contents`]로 연다.
    fn read_layer(
        &self,
        layer: &Layer,
        index: usize,
    ) -> Result<(Vec<LayerEntry>, Arc<dyn Contents>), BuildError>;

    /// 임시 파일을 둘 디렉터리(층 tar를 쓴다). 빌드가 끝나면 비운다.
    fn work_dir(&self) -> &Path;

    /// 만든 이미지를 저장한다. 이미지 ID를 돌려준다.
    fn commit(&self, image: &Built) -> Result<ImageId, BuildError>;
}

/// 빌드 VM을 띄우는 쪽. 실행 코어가 채운다. 네트워크, CPU, 메모리는 바이너리가 정해 둔다.
pub trait Machines: Send + Sync {
    /// `layers`를 뿌리로 하는 빌드 VM을 띄운다. 기본 프로세스는 없다(`hold`).
    fn start<'a>(&'a self, layers: &'a [Layer]) -> BoxFuture<'a, Result<Box<dyn Vm>, BuildError>>;
}

/// `RUN` 하나의 실행 정보.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSpec {
    /// argv.
    pub args: Vec<String>,
    /// 환경. 이 값만 쓴다(이미지의 것을 다시 섞지 않는다).
    pub env: Vec<(String, String)>,
    /// 작업 디렉터리(절대 경로).
    pub cwd: String,
    /// 사용자(config `User` 원문). 비었으면 root.
    pub user: String,
}

/// 실행 출력의 흐름.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// 표준 출력.
    Stdout,
    /// 표준 오류.
    Stderr,
}

/// 실행 출력을 받는 쪽.
pub type OutputSink = Arc<dyn Fn(Stream, &[u8]) + Send + Sync>;

/// 파일 내용을 여는 함수. 한 번 부른다.
pub type Open = Box<dyn FnOnce() -> io::Result<Box<dyn Read>> + Send>;

/// VM에 놓을 항목 하나([`Vm::put`]). 경로는 뿌리 기준 상대 경로이고, 부모가 앞에 온다.
pub struct PutEntry {
    /// 상대 경로.
    pub path: String,
    /// 종류.
    pub kind: PutKind,
    /// 권한 비트.
    pub mode: u32,
    /// 소유자.
    pub uid: u32,
    /// 그룹.
    pub gid: u32,
    /// 수정 시각(Unix 초).
    pub mtime: i64,
}

/// [`PutEntry`]의 종류.
pub enum PutKind {
    /// 디렉터리.
    Dir,
    /// 일반 파일. 정확히 `size` 바이트다.
    File {
        /// 크기.
        size: u64,
        /// 내용.
        open: Open,
    },
    /// 심볼릭 링크.
    Symlink(String),
}

/// 빌드 VM 하나. 단계마다 층을 하나씩 쌓는다.
pub trait Vm: Send {
    /// 새 층을 연다. 그 뒤의 실행과 놓기는 그 층에 한다.
    fn open_layer(&mut self) -> BoxFuture<'_, Result<(), BuildError>>;

    /// `RUN` 하나를 실행하고 종료 코드를 돌려준다. 끝나면 그 단계가 남긴 프로세스도 끝난다.
    fn run(&mut self, spec: RunSpec, output: OutputSink) -> BoxFuture<'_, Result<i32, BuildError>>;

    /// 지금 층의 변경을 압축하지 않은 OCI 층 tar로 `tar`에 쓴다.
    fn export<'a>(&'a mut self, tar: &'a Path) -> BoxFuture<'a, Result<(), BuildError>>;

    /// 뿌리에 항목을 놓는다(호스트에서 만든 `COPY` 층을 VM에도 보이게).
    fn put(&mut self, entries: Vec<PutEntry>) -> BoxFuture<'_, Result<(), BuildError>>;

    /// 이 VM이 지금까지 거부한 연결 대상(이름이나 주소). 실패한 단계의 안내에 쓴다.
    fn blocked(&mut self) -> BoxFuture<'_, Vec<String>>;

    /// VM을 끄고 지운다.
    fn finish(self: Box<Self>) -> BoxFuture<'static, Result<(), BuildError>>;
}

/// `.dockerignore`. 바이너리가 `sbx-dockerfile`의 것으로 채운다.
pub trait Ignore: Send + Sync {
    /// 컨텍스트 기준 경로(`/`로 나눈다)가 빠지는지.
    fn is_excluded(&self, path: &str) -> bool;
    /// 예외 패턴(`!`)이 있는지. 없으면 빠진 디렉터리 아래를 보지 않는다.
    fn has_exceptions(&self) -> bool;
}

/// 만든 이미지. 저장소가 이것으로 이미지 기록을 쓴다([`Store::commit`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    /// config 문서(OCI image config JSON). 이미지 ID는 이것의 sha256이다.
    pub config_json: Vec<u8>,
    /// 이미지 기록에 쓰는 실행 기본값.
    pub config: ImageConfig,
    /// 층. 아래부터.
    pub layers: Vec<Layer>,
    /// 아키텍처.
    pub arch: Arch,
}
