use super::*;

use std::collections::HashMap;
use std::future::Future;
use std::io::Write;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use flate2::Compression as GzLevel;
use flate2::write::GzEncoder;

use sbx_image::{ImageError, ResolvedImage};
use sbx_model::{Arch, BoxFuture, Digest, ImageConfig, Layer};
use sbx_rootfs::RootfsError;
use sbx_rootfs::{ContentSource, NodeKind, WriteSeek};
use sbx_store::digest::sha256;

fn ready<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("future did not complete immediately"),
    }
}

fn temp_root(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("sandbox-manage-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    p
}

enum T<'a> {
    Dir(&'a str),
    File(&'a str, &'a [u8]),
    Whiteout(&'a str),
    Symlink(&'a str, &'a str),
}

/// (gzip blob, Layer 기술)
fn layer(entries: &[T<'_>]) -> (Vec<u8>, Layer) {
    layer_with(Compression::Gzip, entries)
}

fn layer_with(compression: Compression, entries: &[T<'_>]) -> (Vec<u8>, Layer) {
    let mut b = tar::Builder::new(Vec::new());
    for e in entries {
        let mut h = tar::Header::new_gnu();
        let (path, data): (&str, &[u8]) = match e {
            T::Dir(p) => {
                h.set_entry_type(tar::EntryType::Directory);
                h.set_mode(0o755);
                (p, b"")
            }
            T::File(p, d) => {
                h.set_entry_type(tar::EntryType::Regular);
                h.set_mode(0o644);
                (p, d)
            }
            T::Whiteout(p) => {
                h.set_entry_type(tar::EntryType::Regular);
                h.set_mode(0o644);
                (p, b"")
            }
            T::Symlink(p, target) => {
                h.set_entry_type(tar::EntryType::Symlink);
                h.set_mode(0o777);
                h.set_link_name(target).unwrap();
                (p, b"")
            }
        };
        h.set_size(data.len() as u64);
        h.set_uid(0);
        h.set_gid(0);
        h.set_mtime(0);
        b.append_data(&mut h, path, data).unwrap();
    }
    let tar = b.into_inner().unwrap();
    let blob = match compression {
        Compression::None => tar.clone(),
        Compression::Gzip => {
            let mut gz = GzEncoder::new(Vec::new(), GzLevel::default());
            gz.write_all(&tar).unwrap();
            gz.finish().unwrap()
        }
        Compression::Zstd => ruzstd::encoding::compress_to_vec(
            tar.as_slice(),
            ruzstd::encoding::CompressionLevel::Fastest,
        ),
    };
    let l = Layer {
        digest: sha256(&blob),
        diff_id: sha256(&tar),
        size: blob.len() as u64,
        compression,
    };
    (blob, l)
}

struct FakeSource {
    image: ResolvedImage,
    blobs: HashMap<Digest, Vec<u8>>,
    /// 마지막 바이트를 바꿔 보낸다(digest가 틀린다).
    tamper: bool,
    /// 끝없이 보낸다(manifest의 크기를 넘는다).
    endless: bool,
}

impl FakeSource {
    fn new(layers: Vec<(Vec<u8>, Layer)>) -> Self {
        let mut blobs = HashMap::new();
        let mut descs = Vec::new();
        for (blob, l) in layers {
            blobs.insert(l.digest.clone(), blob);
            descs.push(l);
        }
        Self {
            image: ResolvedImage {
                id: ImageId::new(sha256(b"manifest")),
                reference: "docker.io/library/test:latest".into(),
                source: "docker.io/library/test".into(),
                platform: Platform { arch: Arch::X86_64 },
                config: ImageConfig {
                    cmd: vec!["/bin/sh".into()],
                    ..Default::default()
                },
                layers: descs,
                ignored_fields: vec![],
                documents: None,
            },
            blobs,
            tamper: false,
            endless: false,
        }
    }
}

impl ImageSource for FakeSource {
    fn resolve<'a>(
        &'a self,
        _reference: &'a str,
        _platform: Platform,
    ) -> BoxFuture<'a, Result<ResolvedImage, ImageError>> {
        let img = self.image.clone();
        Box::pin(async move { Ok(img) })
    }

    fn fetch_blob<'a>(
        &'a self,
        _source: &'a str,
        digest: &'a Digest,
        sink: &'a mut (dyn Write + Send),
    ) -> BoxFuture<'a, Result<u64, ImageError>> {
        Box::pin(async move {
            let mut data = self.blobs[digest].clone();
            if self.tamper
                && let Some(b) = data.last_mut()
            {
                *b ^= 1;
            }
            if self.endless {
                let chunk = vec![0u8; 64 * 1024];
                loop {
                    sink.write_all(&chunk)?;
                }
            }
            sink.write_all(&data)?;
            Ok(data.len() as u64)
        })
    }
}

/// 받은 트리를 "경로 종류 내용" 줄로 쓰는 writer. 병합 결과와 내용 전달을 확인한다.
struct Listing;

impl RootfsBuilder for Listing {
    fn format(&self) -> RootfsFormat {
        RootfsFormat::Erofs
    }

    fn build(
        &self,
        tree: &MergedTree,
        content: &dyn ContentSource,
        out: &mut dyn WriteSeek,
    ) -> Result<(), RootfsError> {
        for (path, node) in tree.iter() {
            match &node.kind {
                NodeKind::Dir => writeln!(out, "{path}/")?,
                NodeKind::File(c) => {
                    let mut s = String::new();
                    content.open(c)?.read_to_string(&mut s)?;
                    writeln!(out, "{path}={s}")?;
                }
                other => writeln!(out, "{path} {other:?}")?,
            }
        }
        Ok(())
    }
}

/// 디스크 하나로 띄울 때의 rootfs: 모든 층을 합친 것.
fn merged_disk(m: &Manager, id: &ImageId) -> PathBuf {
    let r = m.store().image(id).unwrap();
    let chain = sbx_store::stack::chain_ids(&r.layers).pop().unwrap();
    m.store().layout().merged_disk(&chain, RootfsFormat::Erofs)
}

fn manager(root: &std::path::Path, source: FakeSource) -> Manager {
    Manager::new(root, Arc::new(source), vec![Arc::new(Listing)]).unwrap()
}

#[test]
fn pull_stores_verified_blobs_then_record_and_ref() {
    let root = temp_root("pull");
    let l = layer(&[T::File("a", b"1")]);
    let digest = l.1.digest.clone();
    let m = manager(&root, FakeSource::new(vec![l]));
    let info = ready(m.pull("test", Platform { arch: Arch::X86_64 })).unwrap();
    assert_eq!(info.refs, vec!["docker.io/library/test:latest"]);
    assert!(info.prepared.is_empty());
    assert!(m.store().layout().blob(&digest).is_file());
    assert_eq!(m.store().image(&info.record.id).unwrap(), info.record);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn pull_refuses_an_image_above_the_size_limit_before_downloading() {
    let root = temp_root("toolarge");
    let l = layer(&[T::File("a", b"12345")]);
    let size = l.1.size;
    let m = manager(&root, FakeSource::new(vec![l])).with_max_image_bytes(Some(size - 1));
    let e = ready(m.pull("test", Platform { arch: Arch::X86_64 })).unwrap_err();
    assert!(
        matches!(e, ManageError::TooLarge { max, .. } if max == size - 1),
        "{e}"
    );
    assert_eq!(
        std::fs::read_dir(root.join("blobs/sha256")).map_or(0, |d| d.count()),
        0
    );
    let m = m.with_max_image_bytes(Some(size));
    ready(m.pull("test", Platform { arch: Arch::X86_64 })).unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn pull_reports_progress_for_missing_layers_only() {
    let root = temp_root("progress");
    let a = layer(&[T::File("a", b"1")]);
    let b = layer(&[T::File("b", b"22")]);
    let (size_a, size_b) = (a.1.size, b.1.size);
    let events = std::sync::Mutex::new(Vec::new());
    let m = manager(&root, FakeSource::new(vec![a.clone(), b]));
    let record = |p: PullProgress| events.lock().unwrap().push(p);
    ready(m.pull_with("test", Platform { arch: Arch::X86_64 }, &record)).unwrap();
    let got = std::mem::take(&mut *events.lock().unwrap());
    let total = size_a + size_b;
    assert_eq!(
        got.first().copied(),
        Some(PullProgress {
            layers: 2,
            layers_done: 0,
            bytes: total,
            bytes_done: 0
        })
    );
    assert_eq!(
        got.last().copied(),
        Some(PullProgress {
            layers: 2,
            layers_done: 2,
            bytes: total,
            bytes_done: total
        })
    );
    assert!(
        got.windows(2).all(|w| w[0].bytes_done <= w[1].bytes_done),
        "{got:?}"
    );

    // 이미 있는 레이어는 세지 않는다.
    let m = manager(
        &root,
        FakeSource::new(vec![a, layer(&[T::File("c", b"3")])]),
    );
    ready(m.pull_with("test", Platform { arch: Arch::X86_64 }, &record)).unwrap();
    let got = events.lock().unwrap();
    assert_eq!(got.first().map(|p| p.layers), Some(1));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn pull_rejects_tampered_blob_and_writes_nothing() {
    let root = temp_root("tamper");
    let mut src = FakeSource::new(vec![layer(&[T::File("a", b"1")])]);
    src.tamper = true;
    let id = src.image.id.clone();
    let m = manager(&root, src);
    assert!(matches!(
        ready(m.pull("test", Platform { arch: Arch::X86_64 })),
        Err(ManageError::Integrity(_))
    ));
    assert!(!m.store().has_image(&id));
    assert!(m.store().refs().unwrap().is_empty());
    assert_eq!(
        std::fs::read_dir(root.join("blobs/sha256"))
            .unwrap()
            .count(),
        0
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn pull_stops_a_source_that_sends_more_than_the_manifest_size() {
    let root = temp_root("endless");
    let mut src = FakeSource::new(vec![layer(&[T::File("a", b"1")])]);
    src.endless = true;
    let m = manager(&root, src);
    match ready(m.pull("test", Platform { arch: Arch::X86_64 })) {
        Err(ManageError::Integrity(e)) => assert!(e.contains("larger than"), "{e}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        std::fs::read_dir(root.join("blobs/sha256"))
            .unwrap()
            .count(),
        0
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn prepare_merges_layers_and_is_idempotent() {
    let root = temp_root("prepare");
    let l1 = layer(&[
        T::Dir("etc"),
        T::File("etc/hosts", b"old"),
        T::File("etc/gone", b"x"),
    ]);
    // 압축이 섞인 레이어도 차례로 합친다 (gzip, zstd, 압축 없음).
    let l2 = layer_with(
        Compression::Zstd,
        &[T::File("etc/hosts", b"new"), T::Whiteout("etc/.wh.gone")],
    );
    let l3 = layer_with(Compression::None, &[T::Dir("etc")]);
    let m = manager(&root, FakeSource::new(vec![l1, l2, l3]));
    let id = ready(m.pull("test", Platform { arch: Arch::X86_64 }))
        .unwrap()
        .record
        .id;

    assert!(ready(m.prepare(&id, RootfsFormat::Erofs, 1)).unwrap());
    let listing = std::fs::read_to_string(merged_disk(&m, &id)).unwrap();
    assert_eq!(listing, "/\netc/\netc/hosts=new\n");
    assert!(!ready(m.prepare(&id, RootfsFormat::Erofs, 1)).unwrap());
    assert_eq!(m.images().unwrap()[0].prepared, vec![RootfsFormat::Erofs]);
    // 작업 공간이 남지 않는다.
    assert_eq!(std::fs::read_dir(root.join("tmp")).unwrap().count(), 0);
    std::fs::remove_dir_all(root).unwrap();
}

/// 커널 라벨이 있는 이미지.
fn with_kernel(label: &str) -> FakeSource {
    let mut src = FakeSource::new(vec![layer(&[
        T::Dir("boot"),
        T::File("boot/vmlinux-6.1", b"KERNEL"),
        T::Symlink("vmlinux", "boot/vmlinux-6.1"),
    ])]);
    src.image
        .config
        .labels
        .insert(ImageConfig::KERNEL_LABEL.into(), label.into());
    src
}

#[test]
fn prepare_extracts_the_image_kernel() {
    let root = temp_root("kernel");
    let m = manager(&root, with_kernel("/vmlinux"));
    let id = ready(m.pull("test", Platform { arch: Arch::X86_64 }))
        .unwrap()
        .record
        .id;
    let layout = m.store().layout().clone();
    assert!(!m.is_prepared(&id, RootfsFormat::Erofs, 1));
    assert!(ready(m.prepare(&id, RootfsFormat::Erofs, 1)).unwrap());
    assert!(m.is_prepared(&id, RootfsFormat::Erofs, 1));
    assert_eq!(std::fs::read(layout.kernel(&id)).unwrap(), b"KERNEL");
    let lease = m.store().lease(&id, RootfsFormat::Erofs, 1).unwrap();
    assert_eq!(lease.kernel(), Some(layout.kernel(&id).as_path()));
    drop(lease);
    assert!(!ready(m.prepare(&id, RootfsFormat::Erofs, 1)).unwrap());
    // 커널만 없어졌으면 커널만 다시 꺼낸다.
    std::fs::remove_file(layout.kernel(&id)).unwrap();
    assert!(matches!(
        m.store().lease(&id, RootfsFormat::Erofs, 1),
        Err(LeaseError::KernelNotFound)
    ));
    assert!(!m.is_prepared(&id, RootfsFormat::Erofs, 1));
    assert!(ready(m.prepare(&id, RootfsFormat::Erofs, 1)).unwrap());
    assert!(layout.kernel(&id).is_file());
    std::fs::remove_dir_all(&root).unwrap();

    // 라벨이 가리키는 것이 없거나, 일반 파일이 아니거나, 상대 경로면 rootfs도 만들지 않는다.
    for (label, why) in [
        ("/nope", "not found"),
        ("/boot", "not a regular file"),
        ("vmlinux", "absolute"),
    ] {
        let m = manager(&root, with_kernel(label));
        let id = ready(m.pull("test", Platform { arch: Arch::X86_64 }))
            .unwrap()
            .record
            .id;
        match ready(m.prepare(&id, RootfsFormat::Erofs, 1)) {
            Err(ManageError::Kernel { reason, .. }) => assert!(reason.contains(why), "{reason}"),
            other => panic!("{label}: {other:?}"),
        }
        assert!(!m.is_prepared(&id, RootfsFormat::Erofs, 1));
        assert!(!m.store().layout().kernel(&id).exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}

#[test]
fn prepare_detects_diff_id_mismatch() {
    let root = temp_root("diffid");
    let (blob, mut l) = layer(&[T::File("a", b"1")]);
    l.diff_id = sha256(b"something else");
    let m = manager(&root, FakeSource::new(vec![(blob, l)]));
    let id = ready(m.pull("test", Platform { arch: Arch::X86_64 }))
        .unwrap()
        .record
        .id;
    assert!(matches!(
        ready(m.prepare(&id, RootfsFormat::Erofs, 1)),
        Err(ManageError::Integrity(_))
    ));
    assert!(!m.is_prepared(&id, RootfsFormat::Erofs, 1));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn prepare_without_builder_or_image() {
    let root = temp_root("nobuilder");
    let m = Manager::new(
        &root,
        Arc::new(FakeSource::new(vec![layer(&[T::File("a", b"1")])])),
        vec![],
    )
    .unwrap();
    let id = ImageId::new(sha256(b"missing"));
    assert!(matches!(
        ready(m.prepare(&id, RootfsFormat::Erofs, 1)),
        Err(ManageError::BuilderMissing(RootfsFormat::Erofs))
    ));
    let m = manager(&root, FakeSource::new(vec![]));
    assert!(matches!(
        ready(m.prepare(&id, RootfsFormat::Erofs, 1)),
        Err(ManageError::ImageNotFound(_))
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn remove_and_untag() {
    let root = temp_root("remove");
    let m = manager(&root, FakeSource::new(vec![layer(&[T::File("a", b"1")])]));
    let id = ready(m.pull("test", Platform { arch: Arch::X86_64 }))
        .unwrap()
        .record
        .id;
    ready(m.prepare(&id, RootfsFormat::Erofs, 1)).unwrap();

    let lease = m.store().lease(&id, RootfsFormat::Erofs, 1).unwrap();
    assert!(matches!(m.remove(&id), Err(ManageError::InUse(_))));
    assert!(m.store().has_image(&id));
    drop(lease);

    let r = m.untag("docker.io/library/test:latest").unwrap();
    assert_eq!(r.deleted, None);
    assert!(m.store().has_image(&id));
    assert!(matches!(
        m.untag("docker.io/library/test:latest"),
        Err(ManageError::RefNotFound(_))
    ));

    let r = m.remove(&id).unwrap();
    assert_eq!(r.deleted, Some(id.clone()));
    assert!(!m.store().has_image(&id));
    assert!(matches!(m.remove(&id), Err(ManageError::ImageNotFound(_))));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn remove_deletes_only_layers_no_other_image_uses() {
    let root = temp_root("remove-layers");
    let base = layer(&[T::File("base", b"shared")]);
    let only_a = layer(&[T::File("a", b"a")]);
    let only_b = layer(&[T::File("b", b"b")]);
    let (base_d, a_d, b_d) = (
        base.1.digest.clone(),
        only_a.1.digest.clone(),
        only_b.1.digest.clone(),
    );

    let a = manager(&root, FakeSource::new(vec![base.clone(), only_a]));
    let id_a = ready(a.pull("a", Platform { arch: Arch::X86_64 }))
        .unwrap()
        .record
        .id;
    let mut src_b = FakeSource::new(vec![base, only_b]);
    src_b.image.id = ImageId::new(sha256(b"manifest b"));
    src_b.image.reference = "docker.io/library/b:latest".into();
    let b = manager(&root, src_b);
    let id_b = ready(b.pull("b", Platform { arch: Arch::X86_64 }))
        .unwrap()
        .record
        .id;

    let r = a.remove(&id_a).unwrap();
    assert_eq!(r.deleted_layers, vec![a_d.clone()]);
    let layout = a.store().layout();
    assert!(!layout.blob(&a_d).exists());
    assert!(layout.blob(&base_d).is_file());

    // 남은 이미지는 레이어가 다 있으므로 prepare된다.
    assert!(ready(b.prepare(&id_b, RootfsFormat::Erofs, 1)).unwrap());

    let r = b.remove(&id_b).unwrap();
    let mut expected = vec![base_d, b_d];
    expected.sort();
    assert_eq!(r.deleted_layers, expected);
    assert_eq!(std::fs::read_dir(layout.blobs_dir()).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(layout.pulls_dir()).unwrap().count(), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn install_guest_init_is_content_addressed_and_repairs_damage() {
    let root = temp_root("guest");
    let m = manager(&root, FakeSource::new(vec![]));
    let a = m.install_guest_init(Arch::Aarch64, b"guest one").unwrap();
    assert_eq!(std::fs::read(&a).unwrap(), b"guest one");
    assert!(a.starts_with(root.join("guest")));
    assert!(a.components().any(|c| c.as_os_str() == "arm64"), "{a:?}");
    // 같은 내용이면 같은 경로, 다른 내용이면 다른 경로.
    assert_eq!(
        m.install_guest_init(Arch::Aarch64, b"guest one").unwrap(),
        a
    );
    let b = m.install_guest_init(Arch::Aarch64, b"guest two").unwrap();
    assert_ne!(a, b);
    // 깨진 파일은 다시 쓴다.
    std::fs::write(&a, b"broken").unwrap();
    m.install_guest_init(Arch::Aarch64, b"guest one").unwrap();
    assert_eq!(std::fs::read(&a).unwrap(), b"guest one");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cache_changes_are_recorded_as_events() {
    let root = temp_root("events");
    let m = manager(&root, FakeSource::new(vec![layer(&[T::File("a", b"1")])]));
    let mut r = events_log(&root)
        .reader(sbx_events::Start::Beginning)
        .unwrap();
    let id = ready(m.pull("test", Platform { arch: Arch::X86_64 }))
        .unwrap()
        .record
        .id;
    assert!(ready(m.prepare(&id, RootfsFormat::Erofs, 1)).unwrap());
    // 이미 있으면 아무것도 만들지 않고 기록도 없다.
    assert!(!ready(m.prepare(&id, RootfsFormat::Erofs, 1)).unwrap());
    m.remove(&id).unwrap();
    // 실패한 동작은 기록하지 않는다.
    assert!(m.untag("docker.io/library/nope:latest").is_err());

    let got: Vec<_> = std::iter::from_fn(|| r.next_event().unwrap()).collect();
    let seen: Vec<(&str, Option<&str>)> = got
        .iter()
        .map(|e| {
            let attr = e
                .attributes
                .get("reference")
                .or(e.attributes.get("rootfs_format"));
            (e.action.as_str(), attr.map(String::as_str))
        })
        .collect();
    let reference = "docker.io/library/test:latest";
    assert_eq!(
        seen,
        [
            ("pull", Some(reference)),
            ("prepare", Some("erofs")),
            ("untag", Some(reference)),
            ("delete", None),
        ]
    );
    assert!(
        got.iter()
            .all(|e| e.kind == EVENT_KIND && e.id == id.to_string())
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// 같은 캐시를 쓰는 두 번째 이미지(다른 ID, 다른 참조).
fn second(layers: Vec<(Vec<u8>, Layer)>, name: &str) -> FakeSource {
    let mut src = FakeSource::new(layers);
    src.image.id = ImageId::new(sha256(name.as_bytes()));
    src.image.reference = format!("docker.io/library/{name}:latest");
    src
}

fn pulled(m: &Manager) -> ImageId {
    ready(m.pull("x", Platform { arch: Arch::X86_64 }))
        .unwrap()
        .record
        .id
}

fn layer_disk(m: &Manager, l: &Layer) -> PathBuf {
    m.store()
        .layout()
        .layer_disk(&l.diff_id, RootfsFormat::Erofs)
}

#[test]
fn layers_get_one_disk_each_and_images_share_them() {
    let root = temp_root("share");
    let base = layer(&[T::Dir("etc"), T::File("etc/os", b"base")]);
    let only_a = layer(&[T::Dir("etc"), T::Whiteout("etc/.wh.os"), T::File("a", b"a")]);
    let only_b = layer(&[T::File("b", b"b")]);
    let (bl, al, bbl) = (base.1.clone(), only_a.1.clone(), only_b.1.clone());
    let a = manager(&root, FakeSource::new(vec![base.clone(), only_a]));
    let b = manager(&root, second(vec![base, only_b], "b"));
    let (id_a, id_b) = (pulled(&a), pulled(&b));

    assert!(ready(a.prepare(&id_a, RootfsFormat::Erofs, 8)).unwrap());
    assert_eq!(
        std::fs::read_to_string(layer_disk(&a, &bl)).unwrap(),
        "/\netc/\netc/os=base\n"
    );
    // whiteout은 overlay의 문자 장치 0/0이다.
    assert_eq!(
        std::fs::read_to_string(layer_disk(&a, &al)).unwrap(),
        "/\na=a\netc/\netc/os CharDevice { major: 0, minor: 0 }\n"
    );
    let base_disk = std::fs::metadata(layer_disk(&a, &bl))
        .unwrap()
        .modified()
        .unwrap();
    // b는 자기 층만 만든다. 바탕 층은 a가 만든 것 그대로다.
    assert!(ready(b.prepare(&id_b, RootfsFormat::Erofs, 8)).unwrap());
    assert_eq!(
        std::fs::metadata(layer_disk(&b, &bl))
            .unwrap()
            .modified()
            .unwrap(),
        base_disk
    );
    assert!(layer_disk(&b, &bbl).is_file());
    assert!(!root.join("merged").exists(), "nothing merged");
    let lease = b.store().lease(&id_b, RootfsFormat::Erofs, 8).unwrap();
    assert_eq!(lease.disks(), [layer_disk(&b, &bl), layer_disk(&b, &bbl)]);
    drop(lease);
    assert_eq!(a.images().unwrap()[0].prepared, vec![RootfsFormat::Erofs]);

    // 바탕 층은 하나로 센다. 두 이미지가 쓴다.
    let parts = a.stored_parts().unwrap();
    let base_part = parts
        .iter()
        .find(|p| p.path == b.store().layout().layer_dir(&bl.diff_id))
        .unwrap();
    assert_eq!(base_part.kind, PartKind::Layer);
    let mut both = vec![id_a.clone(), id_b.clone()];
    both.sort();
    assert_eq!(base_part.users, both);
    assert_eq!(
        parts.iter().filter(|p| p.kind == PartKind::Layer).count(),
        3
    );
    assert_eq!(
        parts.iter().filter(|p| p.kind == PartKind::Image).count(),
        2
    );

    // a를 지우면 a만 쓰던 층만 지워진다. 둘 다 지우면 층이 남지 않는다.
    let r = a.remove(&id_a).unwrap();
    assert_eq!(r.deleted_disks, vec![al.diff_id.clone()]);
    assert!(layer_disk(&b, &bl).is_file());
    let r = b.remove(&id_b).unwrap();
    let mut gone = vec![bl.diff_id, bbl.diff_id];
    gone.sort();
    assert_eq!(r.deleted_disks, gone);
    assert_eq!(std::fs::read_dir(root.join("layers")).unwrap().count(), 0);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn layers_above_the_disk_limit_are_merged_from_the_bottom() {
    let root = temp_root("limit");
    let names = ["f0", "f1", "f2", "f3"];
    let ls: Vec<_> = names.iter().map(|n| layer(&[T::File(n, b"x")])).collect();
    let descs: Vec<Layer> = ls.iter().map(|l| l.1.clone()).collect();
    let m = manager(&root, FakeSource::new(ls));
    let id = pulled(&m);
    assert!(ready(m.prepare(&id, RootfsFormat::Erofs, 2)).unwrap());
    let layout = m.store().layout();
    let chain = sbx_store::stack::chain_ids(&descs)[2].clone();
    assert_eq!(
        std::fs::read_to_string(layout.merged_disk(&chain, RootfsFormat::Erofs)).unwrap(),
        "/\nf0=x\nf1=x\nf2=x\n"
    );
    // 합쳐진 층은 따로 만들지 않는다.
    assert!(!layout.layer_dir(&descs[0].diff_id).exists());
    assert!(layer_disk(&m, &descs[3]).is_file());
    assert!(m.is_prepared(&id, RootfsFormat::Erofs, 2));
    assert!(!m.is_prepared(&id, RootfsFormat::Erofs, 8));
    // 디스크가 넉넉한 실행기로 띄우려면 나머지 층을 만든다.
    assert!(ready(m.prepare(&id, RootfsFormat::Erofs, 8)).unwrap());
    assert!(!ready(m.prepare(&id, RootfsFormat::Erofs, 8)).unwrap());
    // 합친 것은 캐시다. 지워도 다시 만든다.
    std::fs::remove_dir_all(layout.merged_for(&chain)).unwrap();
    assert!(!m.is_prepared(&id, RootfsFormat::Erofs, 2));
    assert!(ready(m.prepare(&id, RootfsFormat::Erofs, 2)).unwrap());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_layer_that_needs_the_lower_layers_is_merged_with_them() {
    let root = temp_root("needs-lower");
    // 둘째 층은 tmp/의 항목 없이 tmp/x만 적는다. 따로 쌓으면 아래층 /tmp의 권한을 잃는다.
    let ls = vec![
        layer(&[T::Dir("tmp")]),
        layer(&[T::File("tmp/x", b"x")]),
        layer(&[T::File("top", b"t")]),
    ];
    let descs: Vec<Layer> = ls.iter().map(|l| l.1.clone()).collect();
    let m = manager(&root, FakeSource::new(ls));
    let id = pulled(&m);
    assert!(ready(m.prepare(&id, RootfsFormat::Erofs, 8)).unwrap());
    let layout = m.store().layout();
    let why = std::fs::read_to_string(layout.layer_needs_lower(&descs[1].diff_id)).unwrap();
    assert!(why.contains("tmp"), "{why}");
    let chain = sbx_store::stack::chain_ids(&descs)[1].clone();
    let lease = m.store().lease(&id, RootfsFormat::Erofs, 8).unwrap();
    assert_eq!(
        lease.disks(),
        [
            layout.merged_disk(&chain, RootfsFormat::Erofs),
            layer_disk(&m, &descs[2])
        ]
    );
    drop(lease);
    std::fs::remove_dir_all(root).unwrap();
}

/// 만들 때마다 받은 트리를 적고, 겹치게 하려고 잠깐 멈추는 writer.
struct Recording(Arc<std::sync::Mutex<Vec<String>>>);

impl RootfsBuilder for Recording {
    fn format(&self) -> RootfsFormat {
        RootfsFormat::Erofs
    }

    fn build(
        &self,
        tree: &MergedTree,
        content: &dyn ContentSource,
        out: &mut dyn WriteSeek,
    ) -> Result<(), RootfsError> {
        let mut listing = std::io::Cursor::new(Vec::new());
        Listing.build(tree, content, &mut listing)?;
        let listing = listing.into_inner();
        self.0
            .lock()
            .unwrap()
            .push(String::from_utf8(listing.clone()).unwrap());
        std::thread::sleep(std::time::Duration::from_millis(100));
        out.write_all(&listing)?;
        Ok(())
    }
}

#[test]
fn concurrent_prepares_build_a_shared_layer_once() {
    let root = temp_root("concurrent");
    let base = layer(&[T::File("shared", b"base")]);
    let built = Arc::new(std::sync::Mutex::new(Vec::new()));
    let make = |src: FakeSource| {
        Manager::new(
            &root,
            Arc::new(src),
            vec![Arc::new(Recording(built.clone()))],
        )
        .unwrap()
    };
    let a = make(FakeSource::new(vec![base.clone()]));
    let b = make(second(vec![base.clone()], "b"));
    let (id_a, id_b) = (pulled(&a), pulled(&b));
    let gate = std::sync::Barrier::new(2);
    std::thread::scope(|s| {
        s.spawn(|| {
            gate.wait();
            ready(a.prepare(&id_a, RootfsFormat::Erofs, 8)).unwrap();
        });
        s.spawn(|| {
            gate.wait();
            ready(b.prepare(&id_b, RootfsFormat::Erofs, 8)).unwrap();
        });
    });
    assert_eq!(*built.lock().unwrap(), ["/\nshared=base\n"]);
    assert_eq!(
        std::fs::read_to_string(layer_disk(&a, &base.1)).unwrap(),
        "/\nshared=base\n"
    );
    // 임시 파일이 남지 않는다.
    let mut names: Vec<_> = std::fs::read_dir(a.store().layout().layer_dir(&base.1.diff_id))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, [".lock", "layer.erofs"]);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_layout_1_cache_is_migrated_and_prepared_again() {
    let root = temp_root("migrate");
    let m = manager(&root, FakeSource::new(vec![layer(&[T::File("a", b"1")])]));
    let id = pulled(&m);
    // 0.2가 만든 이미지별 rootfs.
    let old = m.store().layout().legacy_rootfs(&id, RootfsFormat::Erofs);
    std::fs::write(&old, b"merged by 0.2").unwrap();
    assert!(!m.is_prepared(&id, RootfsFormat::Erofs, 8));
    assert_eq!(
        migrate_from_layout_1(&root).unwrap(),
        std::slice::from_ref(&old)
    );
    assert!(!old.exists());
    assert!(ready(m.prepare(&id, RootfsFormat::Erofs, 8)).unwrap());
    assert!(m.is_prepared(&id, RootfsFormat::Erofs, 8));
    // blob이 없으면(손으로 지웠으면) 다시 받으라고 한다.
    let layout = m.store().layout();
    let r = m.store().image(&id).unwrap();
    std::fs::remove_dir_all(layout.layer_dir(&r.layers[0].diff_id)).unwrap();
    std::fs::remove_file(layout.blob(&r.layers[0].digest)).unwrap();
    let e = ready(m.prepare(&id, RootfsFormat::Erofs, 8)).unwrap_err();
    assert!(e.to_string().contains("pull the image again"), "{e}");
    std::fs::remove_dir_all(root).unwrap();
}

/// 받은 문서가 있는 이미지(레이어 둘, 같은 레이어 한 번 더). manifest의 sha256이 이미지 ID다.
fn with_documents() -> FakeSource {
    let a = layer(&[T::File("a", b"1")]);
    let b = layer(&[T::File("b", b"22")]);
    let mut src = FakeSource::new(vec![a.clone(), b, a]);
    let config = b"{\"config\": {}}".to_vec();
    let manifest = b"{ \"kept\": \"as pulled\" }".to_vec();
    src.image.id = ImageId::new(sha256(&manifest));
    src.image.documents = Some(sbx_image::Documents {
        manifest,
        manifest_media_type: "application/vnd.oci.image.manifest.v1+json".into(),
        config_digest: sha256(&config),
        config,
    });
    src
}

/// 받은 것을 기록하는 올릴 곳. `fail_at`번째(0부터) blob에서 실패한다.
#[derive(Default)]
struct FakeDest {
    calls: std::sync::Mutex<Vec<String>>,
    blobs: std::sync::Mutex<HashMap<Digest, Vec<u8>>>,
    manifest: std::sync::Mutex<Option<(String, String, Vec<u8>)>>,
    fail_at: Option<usize>,
    /// 이 digest는 이미 있다.
    present: Vec<Digest>,
}

impl ImageDestination for FakeDest {
    fn target(
        &self,
        reference: &str,
        blobs_from: Option<&str>,
    ) -> Result<sbx_image::PushTarget, ImageError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("target {reference} {blobs_from:?}"));
        Ok(sbx_image::PushTarget {
            reference: format!("normalized/{reference}"),
            mount_from: None,
        })
    }

    fn push_blob<'a>(
        &'a self,
        target: &'a sbx_image::PushTarget,
        digest: &'a Digest,
        content: &'a dyn sbx_image::BlobContent,
        progress: &'a (dyn Fn(u64) + Sync),
    ) -> BoxFuture<'a, Result<BlobPush, ImageError>> {
        Box::pin(async move {
            let n = {
                let mut c = self.calls.lock().unwrap();
                c.push(format!("blob {} {digest}", target.reference));
                c.iter().filter(|l| l.starts_with("blob ")).count() - 1
            };
            if self.fail_at == Some(n) {
                return Err(ImageError::Transport("connection reset".into()));
            }
            if self.present.contains(digest) {
                return Ok(BlobPush::Exists);
            }
            let mut buf = Vec::new();
            content.open(0)?.read_to_end(&mut buf)?;
            progress(buf.len() as u64);
            self.blobs.lock().unwrap().insert(digest.clone(), buf);
            Ok(BlobPush::Uploaded)
        })
    }

    fn push_manifest<'a>(
        &'a self,
        target: &'a sbx_image::PushTarget,
        media_type: &'a str,
        manifest: &'a [u8],
    ) -> BoxFuture<'a, Result<(), ImageError>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap()
                .push(format!("manifest {}", target.reference));
            *self.manifest.lock().unwrap() = Some((
                target.reference.clone(),
                media_type.to_owned(),
                manifest.to_vec(),
            ));
            Ok(())
        })
    }
}

#[test]
fn push_sends_each_blob_once_as_stored_then_the_manifest() {
    let root = temp_root("push");
    let src = with_documents();
    let docs = src.image.documents.clone().unwrap();
    let layers = src.image.layers.clone();
    let m = manager(&root, src);
    let id = ready(m.pull("test", Platform { arch: Arch::X86_64 }))
        .unwrap()
        .record
        .id;
    let mut r = events_log(&root).reader(sbx_events::Start::End).unwrap();
    let dest = FakeDest {
        present: vec![layers[1].digest.clone()],
        ..FakeDest::default()
    };
    let seen = std::sync::Mutex::new(Vec::new());
    let pushed =
        ready(m.push_with(&dest, &id, "reg/x:1", &|p| seen.lock().unwrap().push(p))).unwrap();
    assert_eq!(pushed.reference, "normalized/reg/x:1");
    assert_eq!(pushed.id, id);
    assert_eq!(pushed.manifest_size, docs.manifest.len() as u64);
    // 같은 레이어는 한 번, 그다음 config. 마지막이 manifest.
    let order: Vec<&Digest> = pushed.blobs.iter().map(|b| &b.digest).collect();
    assert_eq!(
        order,
        [&layers[0].digest, &layers[1].digest, &docs.config_digest]
    );
    assert_eq!(
        pushed
            .blobs
            .iter()
            .map(|b| b.how.clone())
            .collect::<Vec<_>>(),
        [BlobPush::Uploaded, BlobPush::Exists, BlobPush::Uploaded]
    );
    let calls = dest.calls.lock().unwrap().clone();
    assert_eq!(
        calls.first().unwrap(),
        "target reg/x:1 Some(\"docker.io/library/test\")"
    );
    assert_eq!(calls.last().unwrap(), "manifest normalized/reg/x:1");
    assert_eq!(
        dest.manifest.lock().unwrap().clone().unwrap(),
        (
            "normalized/reg/x:1".to_owned(),
            docs.manifest_media_type.clone(),
            docs.manifest.clone()
        )
    );
    assert_eq!(dest.blobs.lock().unwrap()[&docs.config_digest], docs.config);
    // 진행: 처음 한 번(0), 끝에 모두.
    let seen = seen.lock().unwrap();
    let total: u64 = layers[0].size + layers[1].size + docs.config.len() as u64;
    assert_eq!(
        seen.first().copied(),
        Some(PushProgress {
            blobs: 3,
            blobs_done: 0,
            bytes: total,
            bytes_done: 0
        })
    );
    assert_eq!(
        seen.last().copied(),
        Some(PushProgress {
            blobs: 3,
            blobs_done: 3,
            bytes: total,
            bytes_done: total
        })
    );
    let e = r.next_event().unwrap().unwrap();
    assert_eq!(
        (e.action.as_str(), e.id.as_str()),
        ("push", id.to_string().as_str())
    );
    assert_eq!(e.attributes["reference"], "normalized/reg/x:1");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_failed_blob_means_no_manifest_and_no_event() {
    let root = temp_root("push-fail");
    let m = manager(&root, with_documents());
    let id = ready(m.pull("test", Platform { arch: Arch::X86_64 }))
        .unwrap()
        .record
        .id;
    let mut r = events_log(&root).reader(sbx_events::Start::End).unwrap();
    let dest = FakeDest {
        fail_at: Some(1),
        ..FakeDest::default()
    };
    let e = ready(m.push(&dest, &id, "reg/x:1")).unwrap_err();
    assert!(e.to_string().contains("connection reset"), "{e}");
    assert!(dest.manifest.lock().unwrap().is_none());
    assert!(
        !dest
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.starts_with("manifest"))
    );
    assert!(r.next_event().unwrap().is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn push_checks_what_it_stored_before_sending_anything() {
    let root = temp_root("push-check");
    // 받은 문서가 없는 이미지(0.2.1 전).
    let m = manager(&root, FakeSource::new(vec![layer(&[T::File("a", b"1")])]));
    let old = ready(m.pull("test", Platform { arch: Arch::X86_64 }))
        .unwrap()
        .record
        .id;
    let dest = FakeDest::default();
    let e = ready(m.push(&dest, &old, "reg/x:1")).unwrap_err();
    assert!(matches!(e, ManageError::NoDocuments(_)), "{e}");
    assert!(e.to_string().contains("pull it again"), "{e}");
    std::fs::remove_dir_all(&root).unwrap();

    let src = with_documents();
    let layer0 = src.image.layers[0].clone();
    let m = manager(&root, src);
    let id = ready(m.pull("test", Platform { arch: Arch::X86_64 }))
        .unwrap()
        .record
        .id;
    let layout = m.store().layout();
    // 잘린 blob.
    let blob = layout.blob(&layer0.digest);
    let good = std::fs::read(&blob).unwrap();
    std::fs::write(&blob, &good[1..]).unwrap();
    let e = ready(m.push(&dest, &id, "reg/x:1")).unwrap_err();
    assert!(matches!(e, ManageError::Integrity(_)), "{e}");
    std::fs::write(&blob, &good).unwrap();
    // 바뀐 manifest.
    let manifest = layout.image_manifest(&id);
    std::fs::write(&manifest, b"{}").unwrap();
    let e = ready(m.push(&dest, &id, "reg/x:1")).unwrap_err();
    assert!(e.to_string().contains("hashes to"), "{e}");
    assert!(dest.calls.lock().unwrap().is_empty());
    // 없는 이미지.
    let e = ready(m.push(&dest, &ImageId::new(sha256(b"none")), "reg/x:1")).unwrap_err();
    assert!(matches!(e, ManageError::ImageNotFound(_)), "{e}");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn tag_adds_or_moves_a_reference_and_records_it() {
    let root = temp_root("tag");
    let m = manager(&root, FakeSource::new(vec![layer(&[T::File("a", b"1")])]));
    let id = ready(m.pull("test", Platform { arch: Arch::X86_64 }))
        .unwrap()
        .record
        .id;
    let mut r = events_log(&root).reader(sbx_events::Start::End).unwrap();
    assert_eq!(m.tag(&id, "example.com/a:1").unwrap(), None);
    let refs = m.store().refs().unwrap();
    assert_eq!(refs["example.com/a:1"], id);
    assert_eq!(refs["docker.io/library/test:latest"], id);
    // 다른 이미지를 가리키던 참조는 옮기고, 그 이미지는 남는다.
    let mut other = FakeSource::new(vec![layer(&[T::File("b", b"2")])]);
    other.image.id = ImageId::new(sha256(b"other"));
    other.image.reference = "docker.io/library/other:latest".into();
    let m2 = manager(&root, other);
    let id2 = ready(m2.pull("other", Platform { arch: Arch::X86_64 }))
        .unwrap()
        .record
        .id;
    assert_eq!(m.tag(&id2, "example.com/a:1").unwrap(), Some(id.clone()));
    assert_eq!(m.store().refs().unwrap()["example.com/a:1"], id2);
    assert!(m.store().has_image(&id));
    // 없는 이미지는 붙이지 않는다.
    assert!(matches!(
        m.tag(&ImageId::new(sha256(b"none")), "example.com/b:1"),
        Err(ManageError::ImageNotFound(_))
    ));
    assert!(!m.store().refs().unwrap().contains_key("example.com/b:1"));
    let got: Vec<_> = std::iter::from_fn(|| r.next_event().unwrap())
        .filter(|e| e.action == "tag")
        .map(|e| (e.id.clone(), e.attributes["reference"].clone()))
        .collect();
    assert_eq!(
        got,
        [
            (id.to_string(), "example.com/a:1".to_owned()),
            (id2.to_string(), "example.com/a:1".to_owned()),
        ]
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// 빌드가 낸 층: 층 디스크만 둔다(blob 없음). 혼자 설 수 없는 층은 tar를 blob으로, 빈 층은 아무것도 두지 않는다.
/// 그런 층으로 된 이미지도 합쳐 띄울 수 있고(디스크를 다시 읽는다), save하면 층을 tar로 다시 만들어 쓴다(몇 번이든
/// 같은 바이트, 각 층을 풀면 그 diff_id). 뿌리 기록은 목록에 없다.
#[test]
fn built_layers_live_on_disks_and_images_made_of_them_prepare() {
    let root = temp_root("built");
    let base = layer(&[T::Dir("etc"), T::File("etc/os", b"base")]);
    let m = Manager::new(
        &root,
        Arc::new(FakeSource::new(vec![base])),
        vec![Arc::new(sbx_rootfs::erofs::ErofsBuilder)],
    )
    .unwrap();
    let base_id = pulled(&m);
    let base_layers = m.store().image(&base_id).unwrap().layers;
    let layout = m.store().layout().clone();
    let tar = |name: &str, entries: &[T<'_>]| {
        let p = root.join(name);
        std::fs::write(&p, layer_with(Compression::None, entries).0).unwrap();
        p
    };
    let (alone, c1) = m
        .add_layer(
            &tar("a.tar", &[T::Dir("app"), T::File("app/x", b"x")]),
            None,
        )
        .unwrap();
    assert!(layer_disk(&m, &alone).is_file());
    assert!(!layout.blob(&alone.digest).exists());
    assert_eq!(
        (alone.digest == alone.diff_id, alone.compression),
        (true, Compression::None)
    );
    let (needs, c2) = m
        .add_layer(&tar("b.tar", &[T::File("etc/y", b"y")]), None)
        .unwrap();
    assert!(layout.layer_needs_lower(&needs.diff_id).is_file());
    assert!(layout.blob(&needs.digest).is_file());
    let (empty, c3) = m.add_layer(&tar("e.tar", &[]), None).unwrap();
    assert_eq!(empty.diff_id.as_str(), sbx_store::EMPTY_LAYER);
    // 디스크에서 다시 읽는다.
    let (entries, contents) = m.layer_entries(&alone, 4).unwrap();
    let x = entries.iter().find(|e| e.path == "app/x").unwrap();
    let sbx_rootfs::EntryKind::File(c) = &x.kind else {
        panic!("{x:?}")
    };
    let mut s = String::new();
    contents.open(c).unwrap().read_to_string(&mut s).unwrap();
    assert_eq!((c.layer, s.as_str()), (4, "x"));
    // 캐시 기록은 층이 있을 때만 쓰인다.
    let key = "ab".repeat(32);
    let rec = sbx_store::BuildCacheRecord {
        layer: alone.clone(),
        created: "2026-01-01T00:00:00Z".into(),
    };
    m.put_build_cache(&key, &rec).unwrap();
    assert_eq!(m.build_cache(&key).unwrap(), Some(rec.clone()));

    // 만든 이미지: 바탕 + 세 층. 디스크 하나로 띄우면 모두 합친다(디스크, blob, 빈 층).
    let mut layers = base_layers.clone();
    layers.extend([alone.clone(), empty.clone(), needs.clone()]);
    let doc = br#"{"config":{}}"#;
    let id = m
        .commit_built(
            doc,
            ImageConfig::default(),
            layers.clone(),
            Arch::X86_64,
            &["docker.io/library/built:latest".into()],
        )
        .unwrap();
    assert_eq!(id.digest(), &sha256(doc));
    assert_eq!(m.config_document(&id).unwrap().unwrap(), doc);
    assert!(ready(m.prepare(&id, RootfsFormat::Erofs, 1)).unwrap());
    let disk = merged_disk(&m, &id);
    let read = sbx_rootfs::erofs::read::read_layer(&mut File::open(&disk).unwrap(), 0).unwrap();
    let mut paths: Vec<&str> = read.entries.iter().map(|e| e.path.as_str()).collect();
    paths.sort();
    assert_eq!(paths, ["app", "app/x", "etc", "etc/os", "etc/y"]);
    // 맨 위가 혼자 설 수 없으므로 디스크가 넷이어도 모두 합친 것 하나로 뜬다(이미 있다).
    assert!(!ready(m.prepare(&id, RootfsFormat::Erofs, 4)).unwrap());
    assert!(m.is_prepared(&id, RootfsFormat::Erofs, 4));
    // save: 층 디스크에서 tar를 다시 만들어 gzip한다. 처음 정한 digest를 기록하고, 다시 해도 같은 바이트다.
    let (mut a, mut b) = (Vec::new(), Vec::new());
    m.save(&id, Some("docker.io/library/built:latest"), &mut a)
        .unwrap();
    assert!(
        layout
            .layer_dir(&alone.diff_id)
            .join(crate::export::EXPORT_RECORD)
            .is_file()
    );
    m.save(&id, Some("docker.io/library/built:latest"), &mut b)
        .unwrap();
    assert_eq!(a, b, "a second save writes the same bytes");
    let files = archive_files(&a);
    let manifest: serde_json::Value = serde_json::from_slice(
        &files[&format!(
            "blobs/sha256/{}",
            m.export_image(&id).unwrap().id.digest().hex()
        )],
    )
    .unwrap();
    let descs = manifest["layers"].as_array().unwrap();
    assert_eq!(descs.len(), layers.len());
    for (d, l) in descs.iter().zip(&layers) {
        let blob = &files[&format!(
            "blobs/sha256/{}",
            Digest::parse(d["digest"].as_str().unwrap()).unwrap().hex()
        )];
        assert_eq!(sha256(blob).as_str(), d["digest"].as_str().unwrap());
        let tar = match d["mediaType"].as_str().unwrap() {
            crate::export::LAYER_GZIP => {
                let mut t = Vec::new();
                flate2::read::GzDecoder::new(&blob[..])
                    .read_to_end(&mut t)
                    .unwrap();
                t
            }
            _ => blob.clone(),
        };
        assert_eq!(sha256(&tar), l.diff_id, "{d}");
    }
    // 저장하지 않은 같은 이미지도 같은 manifest다(`build --output`).
    assert_eq!(
        m.export_built(doc, &layers).unwrap().id,
        m.export_image(&id).unwrap().id
    );
    // 뿌리 기록은 목록에 없고 지울 수 있다.
    let root_id = m.put_build_root(&layers[..2], Arch::X86_64).unwrap();
    assert_eq!(
        m.put_build_root(&layers[..2], Arch::X86_64).unwrap(),
        root_id
    );
    let listed: Vec<ImageId> = m
        .images()
        .unwrap()
        .into_iter()
        .map(|i| i.record.id)
        .collect();
    assert!(
        listed.contains(&id) && !listed.contains(&root_id),
        "{listed:?}"
    );
    assert!(m.remove_build_root(&root_id).unwrap());
    assert!(!m.remove_build_root(&id).unwrap(), "not a build root");
    // 만든 이미지를 지워도 캐시가 가리키는 층은 남는다. 캐시를 지우면 다음 정리가 지운다.
    drop((c1, c2, c3));
    m.remove(&id).unwrap();
    assert!(layer_disk(&m, &alone).is_file());
    assert!(!layout.layer_dir(&needs.diff_id).exists());
    let usage = m.build_cache_usage().unwrap();
    assert_eq!((usage.records, usage.only_cache.len()), (1, 1));
    assert_eq!(m.prune_build_cache().unwrap(), 1);
    m.collect().unwrap();
    assert!(!layout.layer_dir(&alone.diff_id).exists());
    assert_eq!(m.build_cache(&key).unwrap(), None);
    std::fs::remove_dir_all(root).unwrap();
}

/// 만든 이미지의 바탕 층: 바탕 이미지를 `rmi`해도 만든 이미지의 기록이 blob을 쥐어 같은 manifest로 save·push한다.
/// sbx 밖에서 바탕 층의 blob이 지워졌으면(저장하지 않은 빌드도) 다시 받으라고 하고, 다시 받으면 같은 manifest다.
/// 빌드가 만든 층의 디스크가 바뀌거나 잘리면 그 층이라고 말하고, 처음 내보낼 때 정한 기록은 지우지 않는다.
#[test]
fn a_built_image_exports_after_its_base_is_removed_and_names_the_remedy() {
    let root = temp_root("built-base");
    let base = layer(&[T::Dir("etc"), T::File("etc/os", b"base")]);
    let m = Manager::new(
        &root,
        Arc::new(FakeSource::new(vec![base])),
        vec![Arc::new(sbx_rootfs::erofs::ErofsBuilder)],
    )
    .unwrap();
    let base_id = pulled(&m);
    let base_layers = m.store().image(&base_id).unwrap().layers;
    assert_ne!(base_layers[0].digest, base_layers[0].diff_id, "a gzip blob");
    let layout = m.store().layout().clone();
    let tar = |name: &str, entries: &[T<'_>]| {
        let p = root.join(name);
        std::fs::write(&p, layer_with(Compression::None, entries).0).unwrap();
        p
    };
    let (made, claim) = m
        .add_layer(
            &tar("a.tar", &[T::Dir("app"), T::File("app/x", b"x")]),
            None,
        )
        .unwrap();
    let (other, other_claim) = m
        .add_layer(
            &tar("b.tar", &[T::Dir("app"), T::File("app/y", b"yy")]),
            None,
        )
        .unwrap();
    let mut layers = base_layers.clone();
    layers.push(made.clone());
    let doc = br#"{"config":{}}"#;
    let id = m
        .commit_built(
            doc,
            ImageConfig::default(),
            layers.clone(),
            Arch::X86_64,
            &["docker.io/library/built:latest".into()],
        )
        .unwrap();
    drop(claim);
    let manifest = m.export_image(&id).unwrap().id;
    let mut before = Vec::new();
    m.save(&id, None, &mut before).unwrap();

    // (a) 바탕 이미지를 지우고 정리해도 blob은 남고, save와 push가 같은 manifest다.
    m.remove(&base_id).unwrap();
    m.collect().unwrap();
    assert!(!m.store().has_image(&base_id));
    assert!(layout.blob(&base_layers[0].digest).is_file());
    let mut after = Vec::new();
    m.save(&id, None, &mut after).unwrap();
    assert_eq!(after, before);
    let dest = FakeDest::default();
    let pushed = ready(m.push_with(&dest, &id, "reg/built:1", &|_| {})).unwrap();
    assert_eq!(pushed.id, manifest);
    let sent = dest.manifest.lock().unwrap().clone().unwrap().2;
    assert_eq!(sha256(&sent), *manifest.digest());

    // (b) 받은 층의 blob이 sbx 밖에서 지워졌다: 빌드가 만든 층이라고 하지 않고 다시 받으라고 한다. 저장하지 않은
    // 내보내기도 같다. 다시 받으면(없는 blob만 받는다) 같은 manifest다.
    std::fs::remove_file(layout.blob(&base_layers[0].digest)).unwrap();
    for e in [
        m.export_image(&id).unwrap_err(),
        m.export_built(doc, &layers).unwrap_err(),
    ] {
        let e = e.to_string();
        assert!(e.contains("comes from a pulled image"), "{e}");
        assert!(e.contains("sbx pull"), "{e}");
        assert!(!e.contains("sbx build"), "{e}");
    }
    let again = pulled(&m);
    assert_eq!(again, base_id);
    assert!(layout.blob(&base_layers[0].digest).is_file());
    assert_eq!(m.export_built(doc, &layers).unwrap().id, manifest);

    // (c) 빌드가 만든 층의 디스크가 처음 내보낸 뒤 바뀌었다(다른 층의 디스크로)거나 잘렸다: 그 층이라고 말하고
    // 기록(`export.json`)은 그대로다. 압축기가 바뀌었다고 하지 않는다.
    let record = layout
        .layer_dir(&made.diff_id)
        .join(crate::export::EXPORT_RECORD);
    assert!(record.is_file());
    let disk = layer_disk(&m, &made);
    let good = std::fs::read(&disk).unwrap();
    std::fs::copy(layer_disk(&m, &other), &disk).unwrap();
    let e = m.save(&id, None, &mut Vec::new()).unwrap_err().to_string();
    assert!(
        e.contains("a layer made by sbx build no longer matches"),
        "{e}"
    );
    assert!(e.contains("sbx builder prune -a"), "{e}");
    assert!(!e.contains("compressor"), "{e}");
    assert!(record.is_file(), "a changed disk keeps the record");
    std::fs::write(&disk, &good[..good.len() / 2]).unwrap();
    let e = m.save(&id, None, &mut Vec::new()).unwrap_err().to_string();
    assert!(!e.contains("compressor"), "{e}");
    assert!(record.is_file(), "a truncated disk keeps the record");
    std::fs::write(&disk, &good).unwrap();
    let mut fixed = Vec::new();
    m.save(&id, None, &mut fixed).unwrap();
    assert_eq!(fixed, before);
    drop(other_claim);
    std::fs::remove_dir_all(root).unwrap();
}

/// tar 아카이브의 일반 파일들(경로 → 내용).
fn archive_files(tar: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
    let mut out = std::collections::BTreeMap::new();
    let mut a = tar::Archive::new(tar);
    for e in a.entries().unwrap() {
        let mut e = e.unwrap();
        let path = e.path().unwrap().to_string_lossy().into_owned();
        let mut b = Vec::new();
        e.read_to_end(&mut b).unwrap();
        out.insert(path, b);
    }
    out
}

/// `SOURCE_DATE_EPOCH`: 층의 시각을 누른다. 누른 층은 다른 층이고(diff_id가 다르다), 내보낸 tar의 시각도 눌려 있다.
/// 같은 tar를 두 번 들이면 같은 층이다.
#[test]
fn a_clamp_presses_the_times_of_a_built_layer() {
    let root = temp_root("clamp");
    let m = Manager::new(
        &root,
        Arc::new(FakeSource::new(vec![])),
        vec![Arc::new(sbx_rootfs::erofs::ErofsBuilder)],
    )
    .unwrap();
    let path = root.join("l.tar");
    let mut b = tar::Builder::new(Vec::new());
    for (p, t) in [("app/", 2_000_000_000u64), ("app/x", 5)] {
        let mut h = tar::Header::new_gnu();
        h.set_mtime(t);
        h.set_mode(0o755);
        h.set_uid(0);
        h.set_gid(0);
        if p.ends_with('/') {
            h.set_entry_type(tar::EntryType::Directory);
            h.set_size(0);
            b.append_data(&mut h, p, &[][..]).unwrap();
        } else {
            h.set_size(1);
            b.append_data(&mut h, p, &b"x"[..]).unwrap();
        }
    }
    std::fs::write(&path, b.into_inner().unwrap()).unwrap();
    let (plain, _c1) = m.add_layer(&path, None).unwrap();
    let (again, _c2) = m.add_layer(&path, None).unwrap();
    assert_eq!(plain, again, "the same tar is the same layer");
    let (pressed, _c3) = m.add_layer(&path, Some(1_000)).unwrap();
    assert_ne!(plain.diff_id, pressed.diff_id);
    let disk = m
        .store()
        .layout()
        .layer_disk(&pressed.diff_id, RootfsFormat::Erofs);
    let mut tar = Vec::new();
    crate::export::disk_tar(&disk, &mut tar).unwrap();
    assert_eq!(sha256(&tar), pressed.diff_id);
    let times: Vec<(String, u64)> = tar::Archive::new(&tar[..])
        .entries()
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.path().unwrap().to_string_lossy().into_owned(),
                e.header().mtime().unwrap(),
            )
        })
        .collect();
    assert_eq!(times, [("app/".to_owned(), 1_000), ("app/x".to_owned(), 5)]);
    std::fs::remove_dir_all(root).unwrap();
}

/// 캐시 상한: 캐시 기록만 쥔 층의 합이 상한을 넘으면 마지막으로 쓴 때가 오래된 층부터 지운다. 쓰면(캐시 맞음)
/// 새것이 된다. 이미지도 쓰는 층은 세지도 지우지도 않는다.
#[test]
fn the_build_cache_is_trimmed_oldest_first_below_the_cap() {
    let root = temp_root("trim");
    let m = Manager::new(
        &root,
        Arc::new(FakeSource::new(vec![])),
        vec![Arc::new(sbx_rootfs::erofs::ErofsBuilder)],
    )
    .unwrap();
    let layout = m.store().layout().clone();
    let mut layers = Vec::new();
    for (i, name) in ["a", "b", "c", "d"].iter().enumerate() {
        let p = root.join(format!("{name}.tar"));
        let body = name.repeat(1000 * (i + 1));
        let file = format!("app/{name}");
        std::fs::write(
            &p,
            layer_with(
                Compression::None,
                &[T::Dir("app"), T::File(&file, body.as_bytes())],
            )
            .0,
        )
        .unwrap();
        let (l, c) = m.add_layer(&p, None).unwrap();
        let key = format!("{i}").repeat(64);
        m.put_build_cache(
            &key,
            &sbx_store::BuildCacheRecord {
                layer: l.clone(),
                created: "2026-01-01T00:00:00Z".into(),
            },
        )
        .unwrap();
        // 차례를 분명히: 기록의 수정 시각을 하나씩 늦춘다.
        let f = File::options()
            .append(true)
            .open(layout.build_cache_record(&key).unwrap())
            .unwrap();
        f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1000 + i as u64))
            .unwrap();
        drop(c);
        layers.push((key, l));
    }
    // d는 이미지도 쓴다: 세지 않는다.
    m.commit_built(
        b"{}",
        ImageConfig::default(),
        vec![layers[3].1.clone()],
        Arch::X86_64,
        &[],
    )
    .unwrap();
    // a를 쓴다(캐시 맞음): 가장 새것이 된다.
    assert!(m.build_cache(&layers[0].0).unwrap().is_some());
    // 층마다 1로 센다: a, b, c가 3. 상한 1이면 오래된 b, c를 지운다.
    let size = |p: &std::path::Path| u64::from(p.is_dir());
    let t = m.trim_build_cache(1, &size).unwrap();
    assert_eq!((t.records, t.before, t.after), (2, 3, 1));
    let left = |l: &Layer| layout.layer_dir(&l.diff_id).exists();
    assert!(left(&layers[0].1) && !left(&layers[1].1) && !left(&layers[2].1) && left(&layers[3].1));
    // 상한 안이면 아무것도 하지 않는다.
    let t = m.trim_build_cache(1, &size).unwrap();
    assert_eq!((t.records, t.before, t.after), (0, 1, 1));
    // 0이면 캐시만 쥔 것을 모두. 이미지가 쓰는 층의 기록은 남는다.
    let t = m.trim_build_cache(0, &size).unwrap();
    assert_eq!(t.records, 1);
    assert!(m.build_cache(&layers[3].0).unwrap().is_some());
    std::fs::remove_dir_all(root).unwrap();
}
