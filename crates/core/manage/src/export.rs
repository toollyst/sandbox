//! 이미지 내보내기: OCI 아카이브(`save`, `build --output type=docker|oci`), OCI image layout 디렉터리
//! (`build --output type=oci,dest=DIR`), 레지스트리(`push`, `build --push`). 모두 [`Export`] 하나에서 쓴다.
//!
//! **받은 이미지**: 받은 문서(manifest, config)와 레이어 blob을 바이트 그대로 쓴다. 그래서 다시 들여도 같은 이미지
//! ID다. 보내기 전에 manifest의 sha256이 이미지 ID인지, blob 파일의 크기가 manifest의 것인지 본다.
//!
//! **만든 이미지**(`sbx build`, 저장했든 아니든): 저장소에는 층 디스크만 있다(고른 대상의 형태 하나만, 기획 4.1).
//! 내보낼 때 층마다 이렇게 한다.
//!
//! - blob이 있는 층(바탕 이미지에서 받은 층, 혼자 설 수 없어 tar로 둔 층)은 그 blob 그대로. 받은 층의 blob은 그
//!   층을 쓰는 이미지 기록이 있는 동안 남는다(만든 이미지의 기록도 바탕 층을 적는다. 그래서 바탕 이미지를 `rmi`해도
//!   만든 이미지는 같은 manifest로 내보낸다). sbx 밖에서 blob이 지워진 받은 층은 내보내지 않고 다시 받으라고 한다.
//! - 빈 층은 빈 tar(1024바이트)를 gzip한 것.
//! - 나머지는 층 디스크를 다시 읽어 정해진 모양의 tar(모듈 `canon`)로 쓰고 gzip한다. 그 tar의 sha256이 diff_id와
//!   다르면 내보내지 않는다([`ManageError::Export`]).
//! - gzip은 정해진 설정이다: 수준 [`GZIP_LEVEL`], 머리의 시각 0, 이름 없음, OS 255(`flate2`의 기본). 같은 tar면 같은
//!   바이트다(압축기의 판이 같으면. 판이 바뀌어 다른 바이트가 나오는 것은 테스트 `gzip_is_pinned`가 잡는다).
//! - **처음 내보낼 때 정하고 기록한다**: gzip blob의 digest와 크기를 층 디렉터리의 [`EXPORT_RECORD`]에 쓴다. 그 층을
//!   다시 내보내면 기록을 쓰고(압축하지 않고 manifest를 쓴다), 실제로 보내는 바이트가 기록과 같은지 끝에서 본다.
//!   다르면(압축기가 바뀌었다) 기록을 지우고 오류다. 다시 하면 새로 정한다.
//! - **blob을 저장소에 두지 않는다**: 처음 정할 때는 한 번 압축해 digest와 크기만 재고(디스크에 쓰지 않는다), 보낼 때
//!   다시 압축해 흘린다. 레지스트리에 이미 있으면(`HEAD`) 압축하지 않는다. 그래서 내보내기가 차지하는 디스크는 층마다
//!   작은 기록 파일 하나다.
//! - manifest는 OCI image manifest이고 config는 저장한 config 문서 그대로다. manifest는 층 목록으로 그때 만든다(키
//!   순서가 정해져 있어 같은 층이면 같은 바이트다). 그래서 만든 이미지를 몇 번 내보내도 같은 manifest digest다.
//!
//! 내보내는 동안 이미지(저장한 것이면)와 층을 쥔다. 그동안 `rmi`나 정리가 지우지 않는다.

use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use flate2::GzBuilder;
use sbx_image::{BlobContent, ImageDestination};
use sbx_model::{Compression, Digest, ImageId, Layer, RootfsFormat};
use sbx_store::digest::{HashingWriter, sha256};
use sbx_store::write::BlobClaim;
use sbx_store::{ImageGuard, LeaseError};

use crate::{BUILT_SOURCE, EVENT_KIND, ManageError, Manager, PushProgress, Pushed, PushedBlob};

/// gzip 수준(flate2의 기본과 같다).
pub const GZIP_LEVEL: u32 = 6;

/// 층 디렉터리 안의 내보내기 기록 파일 이름: 처음 내보낼 때 정한 gzip blob의 digest와 크기.
pub const EXPORT_RECORD: &str = "export.json";

/// OCI image manifest.
pub const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
/// OCI image config.
pub const OCI_CONFIG: &str = "application/vnd.oci.image.config.v1+json";
/// OCI 층(gzip).
pub const LAYER_GZIP: &str = "application/vnd.oci.image.layer.v1.tar+gzip";
const LAYER_ZSTD: &str = "application/vnd.oci.image.layer.v1.tar+zstd";
const LAYER_TAR: &str = "application/vnd.oci.image.layer.v1.tar";

/// gzip해서 내보낼 tar.
#[derive(Debug, Clone)]
enum TarSource {
    /// 층 디스크(정해진 모양의 tar로 쓴다). 그 sha256이 `diff_id`여야 한다.
    Disk { path: PathBuf, diff_id: Digest },
    /// 빈 층.
    Empty,
}

/// 내보낼 blob 하나의 내용.
#[derive(Debug, Clone)]
enum Source {
    /// 저장소의 파일 그대로.
    File(PathBuf),
    /// tar를 gzip해 흘린다. `record`가 있으면 끝에서 다르면 지울 기록 파일이다.
    Gzip {
        tar: TarSource,
        record: Option<PathBuf>,
    },
}

/// 내보낼 blob 하나.
#[derive(Debug, Clone)]
struct Blob {
    digest: Digest,
    size: u64,
    source: Source,
}

/// 내보낼 이미지 하나([`Manager::export_image`], [`Manager::export_built`]). 규칙은 모듈 문서에 있다.
pub struct Export {
    /// manifest의 digest. 받은 이미지면 이미지 ID와 같다.
    pub id: ImageId,
    /// manifest.
    pub manifest: Vec<u8>,
    /// manifest의 미디어 형식.
    pub manifest_media_type: String,
    /// config 문서.
    pub config: Vec<u8>,
    /// config의 digest.
    pub config_digest: Digest,
    /// 층들(아래부터, 같은 것이 두 번 올 수 있다)의 blob digest.
    layer_digests: Vec<Digest>,
    /// 같은 것은 한 번.
    blobs: Vec<Blob>,
    /// blob을 받아 온 곳(mount의 단서).
    blobs_from: Option<String>,
    _guard: Option<ImageGuard>,
    _claim: Option<BlobClaim>,
}

impl std::fmt::Debug for Export {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Export")
            .field("id", &self.id)
            .field("layers", &self.layer_digests)
            .finish()
    }
}

impl Export {
    /// 층 blob 바이트 합(같은 것은 한 번).
    pub fn layer_bytes(&self) -> u64 {
        self.blobs.iter().map(|b| b.size).sum()
    }
}

/// 층 디스크를 정해진 모양의 tar로 `out`에 쓴다.
pub(crate) fn disk_tar(disk: &Path, out: impl Write) -> Result<(), ManageError> {
    let mut f = BufReader::new(File::open(disk)?);
    let read = sbx_rootfs::erofs::read::read_layer(&mut f, 0).map_err(|e| ManageError::Layer {
        index: 0,
        reason: format!("layer disk {}: {e}", disk.display()),
    })?;
    let mut contents = sbx_rootfs::erofs::read::DiskContents::new();
    contents.add(0, disk, read.offsets);
    crate::canon::write(&read.entries, &contents, out)?;
    Ok(())
}

/// 층 디스크가 내는 tar의 sha256과 크기.
pub(crate) fn canon_digest(disk: &Path) -> Result<(Digest, u64), ManageError> {
    let mut sink = HashingWriter::new(io::sink());
    disk_tar(disk, &mut sink)?;
    Ok((sink.digest(), sink.byte_count()))
}

/// 정해진 설정의 gzip(모듈 문서).
fn encoder<W: Write>(out: W) -> flate2::write::GzEncoder<W> {
    GzBuilder::new().write(out, flate2::Compression::new(GZIP_LEVEL))
}

/// 빌드가 만든 층의 디스크가 diff_id를 다시 내지 못한다(디스크가 바뀌었거나 깨졌거나, 층을 다르게 쓰는 sbx가 썼다).
/// 다시 빌드만 해서는 고쳐지지 않는다: 같은 diff_id의 층 디스크가 남아 있으면 그것을 그대로 쓰기 때문이다.
fn built_layer_mismatch(disk: &Path, got: &Digest, diff_id: &Digest) -> ManageError {
    ManageError::Export(format!(
        "a layer made by sbx build no longer matches its digest: its disk {} gives a tar with digest {got}, not its diff_id {diff_id} (the disk was changed or damaged, or written by an sbx that stored layers differently); remove what holds the layer (`sbx builder prune -a`, and `sbx rmi` the images built with it) and build again",
        disk.display()
    ))
}

/// 받은 이미지의 층인데 압축 blob이 없다. `sbx pull`은 없는 blob만 다시 받는다.
fn pulled_blob_missing(index: usize, l: &Layer) -> ManageError {
    ManageError::Export(format!(
        "layer {index} ({}) comes from a pulled image and its compressed blob is no longer stored; pull that image again (`sbx pull` fetches only the missing blobs) and retry",
        l.digest
    ))
}

/// tar를 gzip해 `out`에 쓴다. 층 디스크(빌드가 만든 층만 온다)의 tar가 diff_id와 다르면
/// [`built_layer_mismatch`]다.
fn gzip(tar: &TarSource, out: impl Write) -> Result<(), ManageError> {
    let mut gz = encoder(out);
    match tar {
        TarSource::Empty => gz.write_all(&[0u8; 1024])?,
        TarSource::Disk { path, diff_id } => {
            let mut h = HashingWriter::new(&mut gz);
            disk_tar(path, &mut h)?;
            let got = h.digest();
            if &got != diff_id {
                return Err(built_layer_mismatch(path, &got, diff_id));
            }
        }
    }
    gz.finish()?.flush()?;
    Ok(())
}

/// 한 번 압축해 gzip blob의 digest와 크기를 잰다(디스크에 쓰지 않는다).
fn measure(tar: &TarSource) -> Result<(Digest, u64), ManageError> {
    let mut sink = HashingWriter::new(io::sink());
    gzip(tar, &mut sink)?;
    Ok((sink.digest(), sink.byte_count()))
}

fn read_record(path: &Path) -> Option<(Digest, u64)> {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    if v["media_type"] != LAYER_GZIP {
        return None;
    }
    Some((
        Digest::parse(v["digest"].as_str()?).ok()?,
        v["size"].as_u64()?,
    ))
}

fn write_record(path: &Path, digest: &Digest, size: u64) -> io::Result<()> {
    let v = serde_json::json!({
        "media_type": LAYER_GZIP,
        "digest": digest.to_string(),
        "size": size,
    });
    sbx_store::write::atomic_write(path, v.to_string().as_bytes())
}

/// 쓰는 쪽이 보내는 것: 바이트, 끝, 오류.
enum Piece {
    Data(Vec<u8>),
    End,
    Failed(io::Error),
}

/// 다른 스레드가 쓰는 바이트를 읽는다. 버리면 쓰는 쪽이 멈춘다.
///
/// 끝은 쓰는 쪽이 [`Piece::End`]로 알릴 때만이다. 쓰는 쪽의 오류는 그대로 돌려주고, 그 뒤로도(그리고 쓰는 쪽이
/// 끝을 알리지 않고 사라져도) 오류다. 그래서 읽는 쪽([`Verify`])은 끝나지 않은 흐름을 짧은 흐름으로 보지 않는다.
struct Piped {
    rx: Mutex<Receiver<Piece>>,
    buf: Vec<u8>,
    at: usize,
    done: bool,
    /// 한 번 난 오류(다시 읽어도 같은 오류다).
    failed: Option<(io::ErrorKind, String)>,
}

impl Read for Piped {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.at == self.buf.len() {
            if let Some((kind, m)) = &self.failed {
                return Err(io::Error::new(*kind, m.clone()));
            }
            if self.done {
                return Ok(0);
            }
            let next = self.rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
            match next {
                Ok(Piece::Data(chunk)) => {
                    self.buf = chunk;
                    self.at = 0;
                }
                Ok(Piece::End) => self.done = true,
                Ok(Piece::Failed(e)) => {
                    self.failed = Some((e.kind(), e.to_string()));
                    return Err(e);
                }
                Err(_) => {
                    self.failed = Some((
                        io::ErrorKind::BrokenPipe,
                        "the layer writer stopped before the end".into(),
                    ));
                }
            }
        }
        let n = out.len().min(self.buf.len() - self.at);
        out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// [`Piped`]에 보내는 쪽.
struct ChanWriter {
    tx: SyncSender<Piece>,
    buf: Vec<u8>,
}

const CHUNK: usize = 64 << 10;

impl Write for ChanWriter {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(b);
        if self.buf.len() >= CHUNK {
            self.flush()?;
        }
        Ok(b.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK));
        self.tx
            .send(Piece::Data(chunk))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the reader went away"))
    }
}

/// 받은 바이트가 정해 둔 digest와 크기인지 끝에서 본다. 다르면 오류이고 `record`를 지운다.
///
/// 이 대조에 닿는 것은 쓰는 쪽이 끝까지 쓴 흐름뿐이다. 층 디스크가 잘렸거나 바뀌었으면 쓰는 쪽([`gzip`])이 먼저
/// 오류를 내고([`Piped`]가 그것을 그대로 돌려준다) 기록은 그대로 둔다. 그래서 여기서 다르다는 것은 tar는 diff_id와
/// 같은데 gzip한 바이트가 기록과 다르다는 것이다: 압축기가 바뀌었거나 기록 파일이 바뀌었다. 기록을 지우면 다음
/// 내보내기가 새로 정한다.
struct Verify<R> {
    inner: R,
    hash: HashingWriter<io::Sink>,
    digest: Digest,
    size: u64,
    record: Option<PathBuf>,
}

impl<R: Read> Read for Verify<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(out)?;
        if n > 0 {
            self.hash.write_all(&out[..n])?;
            if self.hash.byte_count() <= self.size {
                return Ok(n);
            }
        }
        let got = self.hash.digest();
        if got == self.digest && self.hash.byte_count() == self.size {
            return Ok(n);
        }
        if let Some(r) = &self.record {
            let _ = std::fs::remove_file(r);
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the layer compressed to {got} ({} bytes), not the {} ({} bytes) recorded when it was first exported (the compressor or the record changed); the record was reset, so run the command again",
                self.hash.byte_count(),
                self.digest,
                self.size
            ),
        ))
    }
}

/// gzip해 흘리는 blob.
struct GzBlob {
    tar: TarSource,
    digest: Digest,
    size: u64,
    record: Option<PathBuf>,
}

impl GzBlob {
    fn stream(&self) -> Piped {
        let (tx, rx) = sync_channel(8);
        let tar = self.tar.clone();
        std::thread::spawn(move || {
            let mut w = ChanWriter {
                tx: tx.clone(),
                buf: Vec::with_capacity(CHUNK),
            };
            let r = gzip(&tar, &mut w).and_then(|()| Ok(w.flush()?));
            let _ = tx.send(match r {
                Ok(()) => Piece::End,
                Err(e) => Piece::Failed(io::Error::other(e.to_string())),
            });
        });
        Piped {
            rx: Mutex::new(rx),
            buf: Vec::new(),
            at: 0,
            done: false,
            failed: None,
        }
    }
}

impl BlobContent for GzBlob {
    fn size(&self) -> u64 {
        self.size
    }

    fn open(&self, offset: u64) -> io::Result<Box<dyn Read + Send + Sync>> {
        let mut r = Verify {
            inner: self.stream(),
            hash: HashingWriter::new(io::sink()),
            digest: self.digest.clone(),
            size: self.size,
            record: self.record.clone(),
        };
        if offset > 0 {
            // 앞부분을 버린다(끝의 대조는 그대로).
            io::copy(&mut (&mut r).take(offset), &mut io::sink())?;
        }
        Ok(Box::new(r))
    }
}

/// 파일 blob.
struct FileBlob {
    path: PathBuf,
    size: u64,
}

impl BlobContent for FileBlob {
    fn size(&self) -> u64 {
        self.size
    }

    fn open(&self, offset: u64) -> io::Result<Box<dyn Read + Send + Sync>> {
        use std::io::{Seek, SeekFrom};
        let mut f = File::open(&self.path)?;
        f.seek(SeekFrom::Start(offset))?;
        Ok(Box::new(f))
    }
}

impl Blob {
    fn content(&self) -> Box<dyn BlobContent> {
        match &self.source {
            Source::File(path) => Box::new(FileBlob {
                path: path.clone(),
                size: self.size,
            }),
            Source::Gzip { tar, record } => Box::new(GzBlob {
                tar: tar.clone(),
                digest: self.digest.clone(),
                size: self.size,
                record: record.clone(),
            }),
        }
    }
}

fn layer_media(c: Compression) -> &'static str {
    match c {
        Compression::Gzip => LAYER_GZIP,
        Compression::Zstd => LAYER_ZSTD,
        Compression::None => LAYER_TAR,
    }
}

impl Manager {
    /// 저장한 이미지 하나를 내보낼 준비를 한다(규칙은 모듈 문서). 돌려받은 것이 살아 있는 동안 이미지를 쥔다.
    pub fn export_image(&self, image: &ImageId) -> Result<Export, ManageError> {
        let guard = self.store.guard(image).map_err(|e| match e {
            LeaseError::Io(e) => ManageError::Io(e),
            _ => ManageError::ImageNotFound(image.clone()),
        })?;
        let record = self.store.image(image)?;
        let mut e = if record.source == BUILT_SOURCE {
            let config = self
                .config_document(image)?
                .ok_or_else(|| ManageError::Export(format!("image {image} has no config")))?;
            self.export_built(&config, &record.layers)?
        } else {
            self.export_pulled(image, &record)?
        };
        e._guard = Some(guard);
        Ok(e)
    }

    fn export_pulled(
        &self,
        image: &ImageId,
        record: &sbx_store::ImageRecord,
    ) -> Result<Export, ManageError> {
        let (manifest, config, info) = self.documents(image)?;
        let actual = sha256(&manifest);
        if &actual != image.digest() {
            return Err(ManageError::Integrity(format!(
                "the stored manifest of {image} hashes to {actual}"
            )));
        }
        if sha256(&config) != info.config_digest {
            return Err(ManageError::Integrity(format!(
                "the stored config of {image} does not match {}",
                info.config_digest
            )));
        }
        let layout = self.store.layout();
        let mut blobs: Vec<Blob> = Vec::new();
        for (index, l) in record.layers.iter().enumerate() {
            if blobs.iter().any(|b| b.digest == l.digest) {
                continue;
            }
            let path = layout.blob(&l.digest);
            let size = std::fs::metadata(&path)
                .map_err(|e| ManageError::Layer {
                    index,
                    reason: format!("blob {}: {e}", l.digest),
                })?
                .len();
            if size != l.size {
                return Err(ManageError::Integrity(format!(
                    "layer {}: the stored blob is {size} bytes, the manifest says {}",
                    l.digest, l.size
                )));
            }
            blobs.push(Blob {
                digest: l.digest.clone(),
                size,
                source: Source::File(path),
            });
        }
        Ok(Export {
            id: image.clone(),
            manifest,
            manifest_media_type: info.manifest_media_type,
            config,
            config_digest: info.config_digest,
            layer_digests: record.layers.iter().map(|l| l.digest.clone()).collect(),
            blobs,
            blobs_from: Some(record.source.clone()),
            _guard: None,
            _claim: None,
        })
    }

    /// 만든 이미지(저장하지 않았어도 된다)를 내보낼 준비를 한다: config 문서와 층들. 처음 내보내는 층은 여기서 한 번
    /// 압축해 digest를 정하고 기록한다(규칙은 모듈 문서). 돌려받은 것이 살아 있는 동안 층을 쥔다.
    pub fn export_built(&self, config: &[u8], layers: &[Layer]) -> Result<Export, ManageError> {
        let claim = self.claim_layers(layers)?;
        let layout = self.store.layout();
        let mut blobs: Vec<Blob> = Vec::new();
        let mut descs = Vec::new();
        let mut digests = Vec::new();
        // 받은 이미지(빌드가 만든 것도 뿌리 기록도 아닌 것)들의 층 blob digest. blob이 없는 층에서만 필요하다.
        let mut pulled: Option<std::collections::BTreeSet<Digest>> = None;
        for (index, l) in layers.iter().enumerate() {
            let blob = layout.blob(&l.digest);
            let disk = layout.layer_disk(&l.diff_id, RootfsFormat::Erofs);
            let mut is_pulled = || -> Result<bool, ManageError> {
                // 빌드가 디스크로만 둔 층은 digest가 diff_id이고 압축이 없다. 아니면 받은 층이다. 압축 없이 받은 층은
                // 받은 이미지의 기록으로 안다.
                if l.digest != l.diff_id || l.compression != Compression::None {
                    return Ok(true);
                }
                if pulled.is_none() {
                    let mut set = std::collections::BTreeSet::new();
                    for r in self.store.images()? {
                        if r.source != BUILT_SOURCE && r.source != crate::BUILD_ROOT_SOURCE {
                            set.extend(r.layers.into_iter().map(|l| l.digest));
                        }
                    }
                    pulled = Some(set);
                }
                Ok(pulled.as_ref().is_some_and(|s| s.contains(&l.digest)))
            };
            let (b, media) = if blob.is_file() {
                let size = std::fs::metadata(&blob)?.len();
                (
                    Blob {
                        digest: l.digest.clone(),
                        size,
                        source: Source::File(blob),
                    },
                    layer_media(l.compression),
                )
            } else if sbx_store::stack::is_empty_layer(&l.diff_id) {
                let tar = TarSource::Empty;
                let (digest, size) = measure(&tar)?;
                (
                    Blob {
                        digest,
                        size,
                        source: Source::Gzip { tar, record: None },
                    },
                    LAYER_GZIP,
                )
            } else if is_pulled()? {
                return Err(pulled_blob_missing(index, l));
            } else if disk.is_file() {
                let tar = TarSource::Disk {
                    path: disk,
                    diff_id: l.diff_id.clone(),
                };
                let record = layout.layer_dir(&l.diff_id).join(EXPORT_RECORD);
                let (digest, size) = match read_record(&record) {
                    Some(r) => r,
                    None => {
                        let (d, s) = measure(&tar)?;
                        write_record(&record, &d, s)?;
                        (d, s)
                    }
                };
                (
                    Blob {
                        digest,
                        size,
                        source: Source::Gzip {
                            tar,
                            record: Some(record),
                        },
                    },
                    LAYER_GZIP,
                )
            } else {
                return Err(ManageError::Export(format!(
                    "layer {index} ({}), made by sbx build, is no longer stored; build the image again",
                    l.diff_id
                )));
            };
            descs.push(serde_json::json!({
                "mediaType": media,
                "digest": b.digest.to_string(),
                "size": b.size,
            }));
            digests.push(b.digest.clone());
            if !blobs.iter().any(|x| x.digest == b.digest) {
                blobs.push(b);
            }
        }
        let config_digest = sha256(config);
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": OCI_MANIFEST,
            "config": {
                "mediaType": OCI_CONFIG,
                "digest": config_digest.to_string(),
                "size": config.len(),
            },
            "layers": descs,
        }))
        .map_err(io::Error::other)?;
        Ok(Export {
            id: ImageId::new(sha256(&manifest)),
            manifest,
            manifest_media_type: OCI_MANIFEST.into(),
            config: config.to_vec(),
            config_digest,
            layer_digests: digests,
            blobs,
            blobs_from: None,
            _guard: None,
            _claim: Some(claim),
        })
    }

    /// OCI image layout tar로 `out`에 쓴다(`sbx save`, `build --output type=docker|oci`). `names`(정규화한 참조)는
    /// index의 이름과 Docker `manifest.json`의 `RepoTags`가 된다(`docker load`는 그것만 읽는다). index에는 첫 이름을
    /// 붙인다(`io.containerd.image.name`, 태그는 `org.opencontainers.image.ref.name`). digest 참조는 `RepoTags`에
    /// 넣지 않는다(Docker가 태그로 받지 않는다).
    pub fn write_archive(
        &self,
        e: &Export,
        names: &[String],
        out: &mut dyn Write,
    ) -> Result<(), ManageError> {
        let mut tar = tar::Builder::new(out);
        let mut put = |path: &str, size: u64, r: &mut dyn Read| -> io::Result<()> {
            let mut h = tar::Header::new_ustar();
            h.set_size(size);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, path, r)
        };
        for (path, bytes) in documents(e, names)? {
            put(&path, bytes.len() as u64, &mut &bytes[..])?;
        }
        for b in &e.blobs {
            let content = b.content();
            let mut r = content.open(0)?;
            put(&blob_path(&b.digest), b.size, &mut r)?;
        }
        tar.into_inner()?.flush()?;
        Ok(())
    }

    /// OCI image layout 디렉터리로 쓴다(`build --output type=oci,dest=DIR/`). 디렉터리가 없으면 만든다. 그 안의
    /// `index.json`은 이 이미지 하나로 바꾼다. blob은 이미 같은 이름이 있으면 두고, 아니면 옆에 쓴 뒤 이름을 바꾼다.
    pub fn write_layout(
        &self,
        e: &Export,
        names: &[String],
        dir: &Path,
    ) -> Result<(), ManageError> {
        std::fs::create_dir_all(dir.join("blobs/sha256"))?;
        let put = |path: &Path, r: &mut dyn Read| -> io::Result<()> {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let temp = sbx_store::write::temp_path(path.parent().unwrap_or(dir), &name);
            let res = (|| {
                let mut f = io::BufWriter::new(File::create(&temp)?);
                io::copy(r, &mut f)?;
                f.into_inner().map_err(|e| e.into_error())?.sync_all()?;
                std::fs::rename(&temp, path)
            })();
            if res.is_err() {
                let _ = std::fs::remove_file(&temp);
            }
            res
        };
        for b in &e.blobs {
            let path = dir.join(blob_path(&b.digest));
            if path.is_file() && std::fs::metadata(&path)?.len() == b.size {
                continue;
            }
            let content = b.content();
            let mut r = content.open(0)?;
            put(&path, &mut r)?;
        }
        // 문서는 blob 뒤에(index.json이 가리키는 것이 먼저 있게). Docker의 manifest.json은 디렉터리에 쓰지 않는다.
        for (path, bytes) in documents(e, names)? {
            if path == DOCKER_MANIFEST {
                continue;
            }
            put(&dir.join(&path), &mut &bytes[..])?;
        }
        Ok(())
    }

    /// `dest`의 `reference`(정규화한 것, 태그 포함)로 올린다(`push`, `build --push`). 순서와 규칙은 크레이트 문서에
    /// 있다. 끝나면 이벤트 `push`를 남긴다.
    pub async fn push_export(
        &self,
        dest: &dyn ImageDestination,
        e: &Export,
        reference: &str,
        progress: &(dyn Fn(PushProgress) + Sync),
    ) -> Result<Pushed, ManageError> {
        let mut blobs: Vec<(Digest, Box<dyn BlobContent>)> = e
            .blobs
            .iter()
            .map(|b| (b.digest.clone(), b.content()))
            .collect();
        blobs.push((e.config_digest.clone(), Box::new(e.config.clone())));
        let target = dest
            .target(reference, e.blobs_from.as_deref())
            .map_err(ManageError::Source)?;
        let mut state = PushProgress {
            blobs: blobs.len(),
            blobs_done: 0,
            bytes: blobs.iter().map(|(_, b)| b.size()).sum(),
            bytes_done: 0,
        };
        progress(state);
        let mut pushed = Vec::new();
        for (digest, content) in &blobs {
            let content = content.as_ref();
            let start = state.bytes_done;
            let how = dest
                .push_blob(&target, digest, content, &|n| {
                    progress(PushProgress {
                        bytes_done: start + n.min(content.size()),
                        ..state
                    })
                })
                .await
                .map_err(ManageError::Source)?;
            state.blobs_done += 1;
            state.bytes_done = start + content.size();
            progress(state);
            pushed.push(PushedBlob {
                digest: digest.clone(),
                size: content.size(),
                how,
            });
        }
        dest.push_manifest(&target, &e.manifest_media_type, &e.manifest)
            .await
            .map_err(ManageError::Source)?;
        self.emit(
            sbx_events::Event::new(EVENT_KIND, "push", e.id.to_string())
                .with("reference", target.reference.clone()),
        );
        Ok(Pushed {
            reference: target.reference,
            id: e.id.clone(),
            manifest_size: e.manifest.len() as u64,
            blobs: pushed,
        })
    }
}

const DOCKER_MANIFEST: &str = "manifest.json";

fn blob_path(d: &Digest) -> String {
    format!("blobs/sha256/{}", d.hex())
}

/// 아카이브의 문서들: `oci-layout`, `index.json`, Docker의 `manifest.json`, manifest와 config blob.
fn documents(e: &Export, names: &[String]) -> io::Result<Vec<(String, Vec<u8>)>> {
    let mut annotations = serde_json::Map::new();
    if let Some(r) = names.first() {
        annotations.insert("io.containerd.image.name".into(), r.clone().into());
        if let Some((_, tag)) = r.rsplit_once(':').filter(|(_, t)| !t.contains('/')) {
            annotations.insert("org.opencontainers.image.ref.name".into(), tag.into());
        }
    }
    let entry = serde_json::json!({
        "mediaType": e.manifest_media_type,
        "digest": e.id.to_string(),
        "size": e.manifest.len(),
        "annotations": annotations,
    });
    let index = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.index.v1+json",
        "manifests": [entry],
    });
    let repo_tags: Vec<&String> = names.iter().filter(|r| !r.contains('@')).collect();
    let docker = serde_json::json!([{
        "Config": blob_path(&e.config_digest),
        "RepoTags": repo_tags,
        "Layers": e.layer_digests.iter().map(blob_path).collect::<Vec<_>>(),
    }]);
    let json = |v: &serde_json::Value| serde_json::to_vec(v).map_err(io::Error::other);
    Ok(vec![
        (
            "oci-layout".into(),
            br#"{"imageLayoutVersion":"1.0.0"}"#.to_vec(),
        ),
        ("index.json".into(), json(&index)?),
        (DOCKER_MANIFEST.into(), json(&docker)?),
        (blob_path(e.id.digest()), e.manifest.clone()),
        (blob_path(&e.config_digest), e.config.clone()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gzip의 바이트가 정해져 있다. 이 값이 바뀌면 압축기(flate2, miniz_oxide)의 판이 바뀐 것이다: 전에 내보낸 층을
    /// 다시 내보내면 기록과 달라 한 번 실패하고 새로 정한다(모듈 문서). 판을 올리기 전에 그것이 괜찮은지 정한다.
    #[test]
    fn gzip_is_pinned() {
        let (d, size) = measure(&TarSource::Empty).unwrap();
        // 섞인 내용 1 MiB(되풀이와 덜 되풀이되는 부분).
        let input: Vec<u8> = (0u32..1 << 20)
            .map(|i| {
                (i.wrapping_mul(2_654_435_761) >> 13) as u8 & if i % 7 == 0 { 0xff } else { 0x0f }
            })
            .collect();
        let mut sink = HashingWriter::new(io::sink());
        let mut gz = encoder(&mut sink);
        gz.write_all(&input).unwrap();
        gz.finish().unwrap();
        assert_eq!(
            [
                (d.to_string(), size),
                (sink.digest().to_string(), sink.byte_count())
            ],
            [
                (
                    "sha256:acb5604b126ac894c1eb11c4575bf2072fea61232a888e453770c79d7ed56419"
                        .to_owned(),
                    35
                ),
                (
                    "sha256:093af6d05fbd7bcc3939520a0d624d7454eb09ddcaaa0bdce51e5f228f7e602f"
                        .to_owned(),
                    101_015
                )
            ],
            "the gzip bytes changed"
        );
    }

    #[test]
    fn a_reader_that_goes_away_stops_the_writer() {
        let b = GzBlob {
            tar: TarSource::Empty,
            digest: measure(&TarSource::Empty).unwrap().0,
            size: measure(&TarSource::Empty).unwrap().1,
            record: None,
        };
        let mut all = Vec::new();
        b.open(0).unwrap().read_to_end(&mut all).unwrap();
        assert_eq!(all.len() as u64, b.size);
        let mut tail = Vec::new();
        b.open(5).unwrap().read_to_end(&mut tail).unwrap();
        assert_eq!(tail, all[5..]);
        drop(b.open(0).unwrap());
        // 기록과 다르면 오류다.
        let wrong = GzBlob {
            digest: sha256(b"x"),
            ..b
        };
        assert!(wrong.open(0).unwrap().read_to_end(&mut Vec::new()).is_err());
    }

    fn record_file(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "sbx-export-{name}-{}-{EXPORT_RECORD}",
            std::process::id()
        ));
        std::fs::write(&p, b"{}").unwrap();
        p
    }

    /// 쓰는 쪽의 오류(층 디스크를 읽지 못한다, diff_id와 다르다)는 그대로 오고, 기록은 지우지 않는다. 다시 읽어도
    /// 오류다(끝난 것처럼 보이지 않는다).
    #[test]
    fn a_writer_error_passes_through_and_keeps_the_record() {
        let record = record_file("writer");
        let disk = record.with_extension("not-a-disk");
        std::fs::write(&disk, vec![7u8; 8192]).unwrap();
        let (digest, size) = measure(&TarSource::Empty).unwrap();
        let b = GzBlob {
            tar: TarSource::Disk {
                path: disk.clone(),
                diff_id: sha256(b"anything"),
            },
            digest,
            size,
            record: Some(record.clone()),
        };
        let direct = measure(&b.tar).unwrap_err().to_string();
        let mut r = b.open(0).unwrap();
        let e = r.read_to_end(&mut Vec::new()).unwrap_err().to_string();
        assert_eq!(e, direct, "the writer's own error, unchanged");
        assert!(!e.contains("compressor"), "{e}");
        assert!(
            r.read(&mut [0u8; 16]).is_err(),
            "still an error, not an end"
        );
        assert!(record.is_file(), "a writer error keeps the record");
        let _ = std::fs::remove_file(&disk);
        let _ = std::fs::remove_file(&record);
    }

    /// 끝까지 쓴 흐름이 기록과 다르면(짧든 길든) 오류이고 기록을 지운다.
    #[test]
    fn a_real_mismatch_resets_the_record() {
        let (digest, size) = measure(&TarSource::Empty).unwrap();
        for (name, d, s) in [
            ("digest", sha256(b"x"), size),
            ("short", digest.clone(), size + 10),
            ("long", digest.clone(), size - 10),
        ] {
            let record = record_file(name);
            let b = GzBlob {
                tar: TarSource::Empty,
                digest: d,
                size: s,
                record: Some(record.clone()),
            };
            let e = b
                .open(0)
                .unwrap()
                .read_to_end(&mut Vec::new())
                .unwrap_err()
                .to_string();
            assert!(e.contains("record was reset"), "{name}: {e}");
            assert!(!record.exists(), "{name}: the record is removed");
        }
    }

    /// 쓰는 쪽이 끝을 알리지 않고 사라지면 오류다(짧은 흐름으로 보고 기록을 지우지 않는다).
    #[test]
    fn a_writer_that_vanishes_is_an_error() {
        let (tx, rx) = sync_channel(1);
        tx.send(Piece::Data(b"abc".to_vec())).unwrap();
        drop(tx);
        let mut p = Piped {
            rx: Mutex::new(rx),
            buf: Vec::new(),
            at: 0,
            done: false,
            failed: None,
        };
        let mut got = Vec::new();
        let e = p.read_to_end(&mut got).unwrap_err();
        assert_eq!(
            (got.as_slice(), e.kind()),
            (&b"abc"[..], io::ErrorKind::BrokenPipe)
        );
    }
}
