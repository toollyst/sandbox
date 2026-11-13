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

/// 층 하나의 파일 내용([`Manager::layer_entries`]). 작업 공간을 쥐고 있고, 버리면 지운다.
pub struct LayerContents {
    /// 작업 공간(버리면 지운다).
    pub(crate) _work: WorkDir,
    /// 내용.
    pub(crate) contents: layer::Contents,
}

impl ContentSource for LayerContents {
    fn open(&self, content: &ContentRef) -> io::Result<Box<dyn Read + '_>> {
        self.contents.open(content)
    }
}

/// 빌드 캐시가 차지하는 것([`Manager::build_cache_usage`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildCacheUsage {
    /// 캐시 기록 수.
    pub records: usize,
    /// 캐시 기록만 가리키는(어느 이미지도 쓰지 않는) 층 디렉터리와 blob. 있는 것만.
    pub only_cache: Vec<PathBuf>,
    /// 캐시 기록과 이미지가 함께 쓰는 층 디렉터리와 blob. 있는 것만.
    pub shared: Vec<PathBuf>,
}
