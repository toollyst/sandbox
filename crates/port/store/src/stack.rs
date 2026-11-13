//! 층 구성: 이미지 하나를 띄울 때 어느 디스크를 어떤 차례로 붙이는지.
//!
//! 저장 단위는 층이다. 층마다 EROFS 하나(`layers/<diff_id>/layer.<형식>`, [`Part::Layer`])를 두고, 같은 층을 쓰는
//! 이미지는 모두 그 파일 하나를 붙인다. 게스트가 그것들을 overlay 아래층으로 쌓는다(`sbx-guest`의 부팅 규칙).
//! 층 디스크의 모양은 `sbx-rootfs`의 `lower` 규칙이다.
//!
//! 층을 디스크 하나로 둘 수 없을 때는 아래 층들을 하나로 합친 EROFS([`Part::Merged`],
//! `merged/<chain_id>/rootfs.<형식>`)를 맨 아래에 붙인다. 합친 것은 캐시다. 지워도 다음 prepare가 다시 만든다.
//! 같은 아래 층들(같은 chain ID)을 쓰는 이미지끼리는 그것도 함께 쓴다. 합치는 때:
//!
//! - 층이 혼자 설 수 없다([`LayerState::NeedsLower`]): 그 층과 그 아래를 모두 합친다.
//! - 층이 디스크 상한보다 많다: 위의 층부터 디스크 하나씩 주고, 마지막 하나에 남은 아래 층들을 합친다.
//!   상한은 실행기가 붙일 수 있는 디스크 수에서 scratch 하나를 뺀 것이고, [`MAX_LAYER_DISKS`]를 넘지 않는다.
//! - 층이 없는 이미지는 빈 rootfs 하나다([`Part::Merged`], `layers: 0`, chain ID는 [`empty_chain`]).
//!
//! 맨 아래 하나만 남았을 때(상한이 1이거나 그 아래가 첫 층뿐일 때) 첫 층이 혼자 설 수 있으면 합치지 않고 층 디스크를
//! 그대로 쓴다. 층 하나를 합친 것과 보이는 모습이 같다.
//!
//! [`plan`]은 위 층부터 본다. 아직 살펴보지 않은 층([`LayerState::Unknown`])을 만나면 그 번호를 돌려준다
//! ([`Plan::Inspect`]). 관리 쪽이 그 층을 살펴(층 디스크나 `needs-lower` 표시를 만들고) 다시 부른다. 그래서 합쳐질
//! 아래 층들은 살펴보지도, 따로 만들지도 않는다.

use sbx_model::{Digest, Layer};

/// 이미지 하나에 붙이는 층 디스크 수의 상한. 실행기가 더 붙일 수 있어도 이만큼만 쓰고 그 아래는 합친다.
///
/// 층마다 부팅 때 블록 장치 하나와 마운트 하나가 늘고, 없는 경로를 찾을 때(실행 파일 찾기, 모듈 찾기) overlay가 층마다
/// 찾아본다. Docker 이미지는 보통 층이 5~15개다. 16이면 대부분의 이미지가 합치지 않고 뜬다.
pub const MAX_LAYER_DISKS: usize = 16;

/// 층 디스크 하나. 아래부터 붙인다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// 층 하나(diff_id).
    Layer(Digest),
    /// 아래 `layers`개 층을 합친 것. `chain`은 그 층들의 chain ID(OCI image spec).
    Merged {
        /// chain ID.
        chain: Digest,
        /// 합친 층 수(아래부터).
        layers: usize,
    },
}

/// 층 하나를 살펴본 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerState {
    /// 층 디스크가 있다.
    Ready,
    /// 혼자 설 수 없다(`needs-lower` 표시). 그 아래와 함께 합친다.
    NeedsLower,
    /// 아직 살펴보지 않았다.
    Unknown,
}

/// [`plan`]의 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// 이 디스크들을 아래부터 붙인다. [`Part::Merged`]는 아직 만들지 않았을 수 있다.
    Parts(Vec<Part>),
    /// 이 번호(아래부터 0)의 층을 먼저 살펴봐야 한다.
    Inspect(usize),
}

/// 층 없는 rootfs의 chain ID 자리: 빈 바이트열의 sha256.
pub fn empty_chain() -> Digest {
    crate::digest::sha256(b"")
}

/// 층마다 그 층까지의 chain ID(OCI image spec): 첫 층은 diff_id, 그 위는 `sha256(앞 chain ID + " " + diff_id)`.
pub fn chain_ids(layers: &[Layer]) -> Vec<Digest> {
    let mut out: Vec<Digest> = Vec::with_capacity(layers.len());
    for l in layers {
        let next = match out.last() {
            None => l.diff_id.clone(),
            Some(prev) => crate::digest::sha256(format!("{prev} {}", l.diff_id).as_bytes()),
        };
        out.push(next);
    }
    out
}

/// 디스크 `max`개(1 이상으로, [`MAX_LAYER_DISKS`]까지로 맞춘다) 안에서의 층 구성. 규칙은 모듈 문서에 있다.
pub fn plan(layers: &[Layer], max: usize, state: impl Fn(&Digest) -> LayerState) -> Plan {
    let chains = chain_ids(layers);
    let merged = |top: usize| Part::Merged {
        chain: chains[top].clone(),
        layers: top + 1,
    };
    if layers.is_empty() {
        return Plan::Parts(vec![Part::Merged {
            chain: empty_chain(),
            layers: 0,
        }]);
    }
    let mut budget = max.clamp(1, MAX_LAYER_DISKS);
    let mut parts = Vec::new();
    let mut i = layers.len();
    while i > 0 {
        i -= 1;
        let s = state(&layers[i].diff_id);
        if budget == 1 && i > 0 {
            parts.push(merged(i));
            break;
        }
        match s {
            LayerState::Ready => {
                parts.push(Part::Layer(layers[i].diff_id.clone()));
                budget -= 1;
            }
            LayerState::NeedsLower => {
                parts.push(merged(i));
                break;
            }
            LayerState::Unknown => return Plan::Inspect(i),
        }
    }
    parts.reverse();
    Plan::Parts(parts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sbx_model::Compression;
    use std::collections::HashMap;

    fn layers(n: usize) -> Vec<Layer> {
        (0..n)
            .map(|i| {
                let d = crate::digest::sha256(format!("layer {i}").as_bytes());
                Layer {
                    digest: d.clone(),
                    diff_id: d,
                    size: 1,
                    compression: Compression::Gzip,
                }
            })
            .collect()
    }

    /// 모든 층을 살펴볼 때까지 돌린다. `alone[i]`가 거짓이면 그 층은 혼자 설 수 없다. 살펴본 층 번호도 돌려준다.
    fn settle(ls: &[Layer], max: usize, alone: &[bool]) -> (Vec<Part>, Vec<usize>) {
        let mut known: HashMap<Digest, LayerState> = HashMap::new();
        let mut inspected = Vec::new();
        loop {
            match plan(ls, max, |d| {
                known.get(d).copied().unwrap_or(LayerState::Unknown)
            }) {
                Plan::Parts(p) => return (p, inspected),
                Plan::Inspect(i) => {
                    inspected.push(i);
                    let s = if alone[i] {
                        LayerState::Ready
                    } else {
                        LayerState::NeedsLower
                    };
                    assert!(known.insert(ls[i].diff_id.clone(), s).is_none());
                }
            }
        }
    }

    fn short(parts: &[Part], ls: &[Layer]) -> Vec<String> {
        parts
            .iter()
            .map(|p| match p {
                Part::Layer(d) => format!("L{}", ls.iter().position(|l| &l.diff_id == d).unwrap()),
                Part::Merged { layers, .. } => format!("M0..{layers}"),
            })
            .collect()
    }

    #[test]
    fn one_disk_per_layer_when_they_fit() {
        let ls = layers(3);
        let (p, inspected) = settle(&ls, 8, &[true; 3]);
        assert_eq!(short(&p, &ls), ["L0", "L1", "L2"]);
        assert_eq!(inspected, [2, 1, 0], "from the top");
    }

    #[test]
    fn too_many_layers_merge_the_lowest_into_the_last_disk() {
        let ls = layers(5);
        let (p, inspected) = settle(&ls, 3, &[true; 5]);
        assert_eq!(short(&p, &ls), ["M0..3", "L3", "L4"]);
        // 합쳐질 층은 살펴보지 않는다.
        assert_eq!(inspected, [4, 3]);
        // 한 개면 모두 합친다. 층 하나짜리 이미지는 합치지 않는다.
        assert_eq!(short(&settle(&ls, 1, &[true; 5]).0, &ls), ["M0..5"]);
        let one = layers(1);
        assert_eq!(short(&settle(&one, 1, &[true]).0, &one), ["L0"]);
        assert_eq!(short(&settle(&one, 1, &[false]).0, &one), ["M0..1"]);
        // 0도 1로 본다. 상한을 넘는 수는 MAX_LAYER_DISKS로 본다.
        assert_eq!(short(&settle(&ls, 0, &[true; 5]).0, &ls), ["M0..5"]);
        let many = layers(MAX_LAYER_DISKS + 2);
        let (p, _) = settle(&many, 1000, &[true; MAX_LAYER_DISKS + 2]);
        assert_eq!(p.len(), MAX_LAYER_DISKS);
        assert_eq!(
            p[0],
            Part::Merged {
                chain: chain_ids(&many)[2].clone(),
                layers: 3
            }
        );
    }

    #[test]
    fn a_layer_that_needs_the_lower_ones_is_merged_with_them() {
        let ls = layers(4);
        let (p, inspected) = settle(&ls, 8, &[true, true, false, true]);
        assert_eq!(short(&p, &ls), ["M0..3", "L3"]);
        assert_eq!(inspected, [3, 2]);
        // 첫 층이 혼자 설 수 있으면 첫 층 디스크를 쓴다.
        let (p, _) = settle(&ls, 2, &[true, true, true, true]);
        assert_eq!(short(&p, &ls), ["M0..3", "L3"]);
    }

    #[test]
    fn an_image_without_layers_is_one_empty_rootfs() {
        assert_eq!(
            plan(&[], 8, |_| LayerState::Unknown),
            Plan::Parts(vec![Part::Merged {
                chain: empty_chain(),
                layers: 0
            }])
        );
    }

    #[test]
    fn chain_ids_follow_the_oci_definition() {
        let ls = layers(2);
        let c = chain_ids(&ls);
        assert_eq!(c[0], ls[0].diff_id);
        assert_eq!(
            c[1],
            crate::digest::sha256(format!("{} {}", ls[0].diff_id, ls[1].diff_id).as_bytes())
        );
    }
}
