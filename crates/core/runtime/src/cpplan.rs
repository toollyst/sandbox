//! `cp`의 대상 규칙. Docker `cp`와 같다. 방향(호스트 → 게스트, 게스트 → 호스트)과 상관없이 같은 표다.
//!
//! | 원본 | 대상 | 결과 |
//! |---|---|---|
//! | 파일 | 없음 | 그 경로에 파일을 만든다 (부모는 있어야 한다) |
//! | 파일 | 없음, `/`로 끝남 | 오류: 대상 디렉터리가 없다 |
//! | 파일 | 파일 | 덮어쓴다 |
//! | 파일 | 디렉터리 | 그 안에 원본 이름으로 |
//! | 디렉터리 | 없음 | 대상 경로로 디렉터리를 만들고 원본의 내용을 넣는다 |
//! | 디렉터리 | 파일 | 오류: 디렉터리를 파일에 복사할 수 없다 |
//! | 디렉터리 | 디렉터리 | 원본이 `/.`로 끝나면 내용을, 아니면 원본 디렉터리를 그 안에 |
//!
//! 결과는 경로를 계산하지 않는다. 경로 규칙(게스트는 `/`, 호스트는 OS마다 다름)은 호출하는 쪽이 안다.

/// 대상 쪽 경로의 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// 없음.
    Missing,
    /// 파일(디렉터리가 아닌 것).
    File,
    /// 디렉터리.
    Dir,
}

/// 놓는 방법.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// 대상 디렉터리 안에, 원본 이름으로.
    Inside,
    /// 대상 디렉터리 안에, 원본 디렉터리의 내용만.
    ContentsInside,
    /// 대상 경로 자체로: 대상의 부모 디렉터리 안에 대상의 이름으로.
    AsTarget,
}

/// 규칙을 적용한다. `contents`는 원본이 `/.`로 끝났는지, `trailing_slash`는 대상이 `/`로 끝났는지다.
pub fn plan(
    src_is_dir: bool,
    contents: bool,
    target: Target,
    trailing_slash: bool,
) -> Result<Placement, &'static str> {
    match (src_is_dir, target) {
        (false, Target::Missing) if trailing_slash => Err("destination directory does not exist"),
        (false, Target::Missing | Target::File) => Ok(Placement::AsTarget),
        (false, Target::Dir) => Ok(Placement::Inside),
        (true, Target::File) => Err("cannot copy a directory to a file"),
        (true, Target::Missing) => Ok(Placement::AsTarget),
        (true, Target::Dir) if contents => Ok(Placement::ContentsInside),
        (true, Target::Dir) => Ok(Placement::Inside),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_cp_table() {
        use Placement::*;
        use Target::*;
        let cases = [
            (false, false, Missing, false, Ok(AsTarget)),
            (false, false, Missing, true, Err(())),
            (false, false, File, false, Ok(AsTarget)),
            (false, false, Dir, false, Ok(Inside)),
            (true, false, Missing, false, Ok(AsTarget)),
            (true, false, File, false, Err(())),
            (true, false, Dir, false, Ok(Inside)),
            (true, true, Dir, false, Ok(ContentsInside)),
        ];
        for (dir, contents, target, slash, expected) in cases {
            let got = plan(dir, contents, target, slash).map_err(|_| ());
            assert_eq!(got, expected, "{dir} {contents} {target:?} {slash}");
        }
    }
}
