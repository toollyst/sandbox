//! `push`: 저장한 이미지를 올린다. 받은 이미지는 바이트 그대로, 만든 이미지는 층을 tar로 다시 만들어(`export` 모듈).
//! 규칙은 크레이트 문서의 "명시적 동작"에 있다.

use sbx_image::ImageDestination;
use sbx_model::ImageId;

use crate::{ManageError, Manager, PushProgress, Pushed};

impl Manager {
    /// 이미지를 `dest`의 `reference`(정규화한 것, 태그 포함)로 올린다. 순서와 규칙은 크레이트 문서에 있다.
    /// 끝나면 이벤트 `push`를 남긴다.
    pub async fn push(
        &self,
        dest: &dyn ImageDestination,
        image: &ImageId,
        reference: &str,
    ) -> Result<Pushed, ManageError> {
        self.push_with(dest, image, reference, &|_| {}).await
    }

    /// [`push`](Self::push)와 같고, 올리는 동안 `progress`에 진행을 알린다. 올릴 blob 목록이 정해지면 한 번, 그 뒤
    /// 바이트를 보내거나 blob 하나가 끝날 때마다 부른다.
    pub async fn push_with(
        &self,
        dest: &dyn ImageDestination,
        image: &ImageId,
        reference: &str,
        progress: &(dyn Fn(PushProgress) + Sync),
    ) -> Result<Pushed, ManageError> {
        let e = self.export_image(image)?;
        self.push_export(dest, &e, reference, progress).await
    }
}
