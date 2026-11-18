//! `sbx build`의 저장소 쪽에서 밖에 보이는 것: 기록의 표시, 층 내용, 캐시 사용량. 규칙은 `build` 모듈에 있다.

use std::io::{self, Read};
use std::path::PathBuf;

use sbx_rootfs::{ContentRef, ContentSource};
use sbx_store::write::WorkDir;

use crate::layer;

/// `sbx build`가 만든 이미지 기록의 `source`.
pub const BUILT_SOURCE: &str = "sbx build";

/// 빌드 VM의 뿌리 기록의 `source`. 이미지 목록에 보이지 않는다.
pub const BUILD_ROOT_SOURCE: &str = "sbx build (working root)";

/// 층 하나의 파일 내용([`Manager::layer_entries`](crate::Manager::layer_entries)). 작업 공간을 쥐고 있고, 버리면 지운다.
pub struct LayerContents {
    /// 작업 공간(버리면 지운다).
    pub(crate) _work: WorkDir,
    /// 내용.
    pub(crate) contents: layer::Contents,
}

impl LayerContents {
    /// 내용을 연다. 돌려받은 것이 이것을 빌리지 않는다(다른 스레드로 넘길 수 있다). 이것을 버리면 작업 공간이
    /// 지워지므로 다 읽을 때까지 쥐고 있는다.
    pub fn open_owned(&self, content: &ContentRef) -> io::Result<Box<dyn Read + Send>> {
        self.contents.open_owned(content)
    }
}

impl ContentSource for LayerContents {
    fn open(&self, content: &ContentRef) -> io::Result<Box<dyn Read + '_>> {
        self.contents.open(content)
    }
}

/// 빌드 캐시가 차지하는 것([`Manager::build_cache_usage`](crate::Manager::build_cache_usage)).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildCacheUsage {
    /// 캐시 기록 수.
    pub records: usize,
    /// 캐시 기록만 가리키는(어느 이미지도 쓰지 않는) 층 디렉터리와 blob. 있는 것만.
    pub only_cache: Vec<PathBuf>,
    /// 캐시 기록과 이미지가 함께 쓰는 층 디렉터리와 blob. 있는 것만.
    pub shared: Vec<PathBuf>,
}

/// 빌드 캐시를 줄인 결과([`Manager::trim_build_cache`](crate::Manager::trim_build_cache)).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trimmed {
    /// 지운 캐시 기록 수.
    pub records: usize,
    /// 줄이기 전에 캐시 기록만 쥔 층이 차지한 바이트(재는 함수로 잰 것).
    pub before: u64,
    /// 줄인 뒤.
    pub after: u64,
}
