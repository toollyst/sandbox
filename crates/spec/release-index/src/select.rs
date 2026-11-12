//! 색인에서 고르기: 알릴 것([`status`])과 `upgrade`의 목표([`target`]). 네트워크에 닿지 않는다.

use crate::index::{Index, Release, Version};

/// 지금 버전에서 본 색인.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status<'a> {
    /// 지금 버전.
    pub current: Version,
    /// 지금보다 높고, 거두지 않았고, 이 플랫폼 묶음이 있는 가장 높은 릴리스.
    pub latest: Option<&'a Release>,
    /// 지금과 `latest` 사이(`latest` 포함)에 보안 고침이 있다.
    pub security: bool,
    /// 색인에 있는 지금 버전(거뒀는지, 알림이 있는지 본다).
    pub this: Option<&'a Release>,
}

impl Status<'_> {
    /// 지금 버전을 거뒀다.
    pub fn yanked(&self) -> bool {
        self.this.is_some_and(|r| r.yanked)
    }

    /// 지금 버전에 붙은 알림.
    pub fn advisory(&self) -> Option<&str> {
        self.this.and_then(|r| r.advisory.as_deref())
    }

    /// 알릴 것이 있다.
    pub fn notable(&self) -> bool {
        self.latest.is_some() || self.yanked() || self.advisory().is_some()
    }

    /// 매번 알려야 할 만큼 급하다(보안 고침, 거둔 버전).
    pub fn urgent(&self) -> bool {
        (self.latest.is_some() && self.security) || self.yanked()
    }
}

/// `current`에서 본 `index`. `platform`의 묶음이 없는 릴리스는 새 버전으로 치지 않는다.
pub fn status<'a>(current: Version, index: &'a Index, platform: &str) -> Status<'a> {
    let latest = index
        .releases
        .iter()
        .filter(|r| r.version > current && !r.yanked && r.assets.contains_key(platform))
        .max_by_key(|r| r.version);
    let security = latest.is_some_and(|l| {
        index
            .releases
            .iter()
            .any(|r| r.version > current && r.version <= l.version && r.security)
    });
    Status {
        current,
        latest,
        security,
        this: index.get(&current),
    }
}

/// `upgrade`의 목표. `None`이면 이미 그 버전이다(또는 더 높은 것이 없다).
///
/// - `want`가 없으면 거두지 않았고 이 플랫폼 묶음이 있는 가장 높은 버전. 지금보다 높지 않으면 `None`.
/// - `want`가 있으면 그 버전(거둔 것이어도 된다, 알리는 것은 부르는 쪽). 지금보다 낮으면 `allow_downgrade`가
///   있어야 한다.
pub fn target<'a>(
    current: Version,
    index: &'a Index,
    platform: &str,
    want: Option<Version>,
    allow_downgrade: bool,
) -> Result<Option<&'a Release>, String> {
    let Some(v) = want else {
        return Ok(status(current, index, platform).latest);
    };
    let r = index
        .get(&v)
        .ok_or_else(|| format!("sbx {v} is not in the release index"))?;
    if !r.assets.contains_key(platform) {
        return Err(format!("sbx {v} has no build for {platform}"));
    }
    if v == current {
        return Ok(None);
    }
    if v < current && !allow_downgrade {
        return Err(format!(
            "sbx {v} is older than this sbx ({current}); pass --allow-downgrade to install it"
        ));
    }
    Ok(Some(r))
}
