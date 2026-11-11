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

    assert!(ready(m.prepare(&id, RootfsFormat::Erofs)).unwrap());
    let listing =
        std::fs::read_to_string(m.store().layout().rootfs(&id, RootfsFormat::Erofs)).unwrap();
    assert_eq!(listing, "/\netc/\netc/hosts=new\n");
    assert!(!ready(m.prepare(&id, RootfsFormat::Erofs)).unwrap());
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
    assert!(!m.is_prepared(&id, RootfsFormat::Erofs));
    assert!(ready(m.prepare(&id, RootfsFormat::Erofs)).unwrap());
    assert!(m.is_prepared(&id, RootfsFormat::Erofs));
    assert_eq!(std::fs::read(layout.kernel(&id)).unwrap(), b"KERNEL");
    let lease = m.store().lease(&id, RootfsFormat::Erofs).unwrap();
    assert_eq!(lease.kernel(), Some(layout.kernel(&id).as_path()));
    drop(lease);
    assert!(!ready(m.prepare(&id, RootfsFormat::Erofs)).unwrap());
    // 커널만 없어졌으면 커널만 다시 꺼낸다.
    std::fs::remove_file(layout.kernel(&id)).unwrap();
    assert!(matches!(
        m.store().lease(&id, RootfsFormat::Erofs),
        Err(LeaseError::KernelNotFound)
    ));
    assert!(!m.is_prepared(&id, RootfsFormat::Erofs));
    assert!(ready(m.prepare(&id, RootfsFormat::Erofs)).unwrap());
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
        match ready(m.prepare(&id, RootfsFormat::Erofs)) {
            Err(ManageError::Kernel { reason, .. }) => assert!(reason.contains(why), "{reason}"),
            other => panic!("{label}: {other:?}"),
        }
        assert!(!m.store().has_rootfs(&id, RootfsFormat::Erofs));
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
        ready(m.prepare(&id, RootfsFormat::Erofs)),
        Err(ManageError::Integrity(_))
    ));
    assert!(!m.store().has_rootfs(&id, RootfsFormat::Erofs));
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
        ready(m.prepare(&id, RootfsFormat::Erofs)),
        Err(ManageError::BuilderMissing(RootfsFormat::Erofs))
    ));
    let m = manager(&root, FakeSource::new(vec![]));
    assert!(matches!(
        ready(m.prepare(&id, RootfsFormat::Erofs)),
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
    ready(m.prepare(&id, RootfsFormat::Erofs)).unwrap();

    let lease = m.store().lease(&id, RootfsFormat::Erofs).unwrap();
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
    assert!(ready(b.prepare(&id_b, RootfsFormat::Erofs)).unwrap());

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
    assert!(ready(m.prepare(&id, RootfsFormat::Erofs)).unwrap());
    // 이미 있으면 아무것도 만들지 않고 기록도 없다.
    assert!(!ready(m.prepare(&id, RootfsFormat::Erofs)).unwrap());
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
