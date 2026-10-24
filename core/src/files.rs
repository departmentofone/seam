//! Files offered to other devices, and files arriving from them.

use std::collections::{HashSet, VecDeque};
use std::io::Read;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::net::Net;
use crate::protocol::{DeviceId, FileEntry, FileOffer, FileRequest};
use crate::win::dataobject::FileSource;

const MAX_ENTRIES: usize = 200_000;
/// Offers kept alive for reading. Older ones expire.
const KEEP_OFFERS: usize = 32;
const CHUNK: usize = 256 * 1024;

struct Registered {
    id: u64,
    paths: Vec<Option<PathBuf>>,
    peers: HashSet<DeviceId>,
    total: u64,
    label: String,
}

#[derive(Default)]
pub struct Offers {
    next: AtomicU64,
    list: Mutex<VecDeque<Registered>>,
}

impl Offers {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { next: AtomicU64::new(rand::random::<u32>() as u64 + 1), list: Mutex::default() })
    }

    /// Walk `roots` and register the result for `peers`. Runs on a blocking
    /// thread: big folders take a moment.
    pub fn create(&self, roots: &[PathBuf], peers: HashSet<DeviceId>) -> anyhow::Result<FileOffer> {
        let mut entries = Vec::new();
        let mut paths = Vec::new();
        for root in roots {
            let name = root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("Drive {}", root.to_string_lossy().trim_end_matches(['\\', ':'])));
            walk(root, &name, &mut entries, &mut paths)?;
        }
        anyhow::ensure!(!entries.is_empty(), "nothing to send");
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let total = entries.iter().map(|e| e.size).sum();
        let files = entries.iter().filter(|e| !e.is_dir).count();
        let label = match (roots.len(), files) {
            (1, _) => roots[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "1 item".into()),
            (n, _) => format!("{n} items"),
        };
        let mut list = self.list.lock();
        list.push_back(Registered { id, paths, peers, total, label });
        while list.len() > KEEP_OFFERS {
            list.pop_front();
        }
        Ok(FileOffer { id, entries })
    }

    /// Register exact files, each under the path it should have on the
    /// other side.
    pub fn create_explicit(&self, files: Vec<(String, PathBuf)>, peers: HashSet<DeviceId>, label: &str) -> anyhow::Result<FileOffer> {
        anyhow::ensure!(files.len() <= 250_000, "that's over 250,000 files; untick some project folders");
        let mut entries = Vec::with_capacity(files.len());
        let mut paths = Vec::with_capacity(files.len());
        for (rel, path) in files {
            let meta = std::fs::metadata(&path)?;
            entries.push(FileEntry { rel, size: meta.len(), is_dir: false, modified: filetime(&meta) });
            paths.push(Some(path));
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let total = entries.iter().map(|e| e.size).sum();
        let mut list = self.list.lock();
        list.push_back(Registered { id, paths, peers, total, label: label.to_string() });
        while list.len() > KEEP_OFFERS {
            list.pop_front();
        }
        Ok(FileOffer { id, entries })
    }

    fn lookup(&self, offer: u64, index: u32, peer: &DeviceId) -> Option<PathBuf> {
        let list = self.list.lock();
        let r = list.iter().find(|r| r.id == offer)?;
        if !r.peers.contains(peer) {
            return None;
        }
        r.paths.get(index as usize)?.clone()
    }

    pub fn describe(&self, offer: u64) -> Option<(String, u64)> {
        self.list.lock().iter().find(|r| r.id == offer).map(|r| (r.label.clone(), r.total))
    }
}

fn filetime(meta: &std::fs::Metadata) -> u64 {
    meta.last_write_time()
}

fn walk(path: &Path, rel: &str, entries: &mut Vec<FileEntry>, paths: &mut Vec<Option<PathBuf>>) -> anyhow::Result<()> {
    anyhow::ensure!(entries.len() < MAX_ENTRIES, "too many files (over {MAX_ENTRIES})");
    let meta = std::fs::symlink_metadata(path)?;
    // Skip symlinks and junctions: they can loop, and the target may be
    // huge. Cloud placeholders (OneDrive) are not links and are kept.
    if meta.file_type().is_symlink() {
        return Ok(());
    }
    if meta.is_dir() {
        entries.push(FileEntry { rel: rel.to_string(), size: 0, is_dir: true, modified: filetime(&meta) });
        paths.push(None);
        let mut children: Vec<_> = std::fs::read_dir(path)?.filter_map(|e| e.ok()).collect();
        children.sort_by_key(|e| e.file_name());
        for child in children {
            let name = child.file_name().to_string_lossy().into_owned();
            if let Err(e) = walk(&child.path(), &format!("{rel}\\{name}"), entries, paths) {
                tracing::debug!("skipping {}: {e}", child.path().display());
            }
        }
    } else if meta.is_file() {
        entries.push(FileEntry { rel: rel.to_string(), size: meta.len(), is_dir: false, modified: filetime(&meta) });
        paths.push(Some(path.to_path_buf()));
    }
    Ok(())
}

/// Send one offered file down `send`. Returns bytes sent.
pub async fn serve(
    offers: &Offers,
    from: DeviceId,
    req: FileRequest,
    mut send: quinn::SendStream,
    progress: impl Fn(u64),
) -> anyhow::Result<u64> {
    let Some(path) = offers.lookup(req.offer, req.index, &from) else {
        let _ = send.reset(1u32.into());
        anyhow::bail!("request for a file that was not offered to this device");
    };
    let mut file = tokio::fs::File::open(&path).await?;
    if req.offset > 0 {
        file.seek(std::io::SeekFrom::Start(req.offset)).await?;
    }
    let mut buf = vec![0u8; CHUNK];
    let mut sent = 0u64;
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        send.write_all(&buf[..n]).await?;
        sent += n as u64;
        progress(n as u64);
    }
    send.finish()?;
    Ok(sent)
}

/// Turn a path from another device into a safe relative path, or refuse it.
pub fn sanitize(rel: &str) -> Option<PathBuf> {
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3",
        "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    let mut out = PathBuf::new();
    for part in rel.split(['\\', '/']) {
        if part.is_empty() || part == "." || part == ".." {
            return None;
        }
        if part.chars().any(|c| c < ' ' || "<>:\"|?*".contains(c)) {
            return None;
        }
        let trimmed = part.trim_end_matches([' ', '.']);
        if trimmed.is_empty() {
            return None;
        }
        let stem = trimmed.split('.').next().unwrap_or("").to_ascii_uppercase();
        if RESERVED.contains(&stem.as_str()) {
            out.push(format!("_{trimmed}"));
        } else {
            out.push(trimmed);
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// `dir\name`, or `dir\name (2).ext` and so on if that is taken.
pub fn unique_path(dir: &Path, name: &Path) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let stem = name.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = name.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let parent = first.parent().map(Path::to_path_buf).unwrap_or_else(|| dir.to_path_buf());
    (2..).map(|n| parent.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap()
}

/// Download every entry of `offer` from `from` into `dest`. Top-level names
/// that already exist get a numbered copy; nothing is ever overwritten.
/// Returns the first top-level item, to reveal in Explorer.
pub async fn receive(net: &Net, from: DeviceId, offer: &FileOffer, dest: &Path, progress: impl Fn(u64)) -> anyhow::Result<PathBuf> {
    tokio::fs::create_dir_all(dest).await?;
    let mut roots: std::collections::HashMap<String, PathBuf> = std::collections::HashMap::new();
    let mut first: Option<PathBuf> = None;

    for (index, e) in offer.entries.iter().enumerate() {
        let rel = sanitize(&e.rel).ok_or_else(|| anyhow::anyhow!("refused unsafe file name {:?}", e.rel))?;
        let mut parts = rel.components();
        let top = parts.next().unwrap().as_os_str().to_string_lossy().into_owned();
        let rest: PathBuf = parts.collect();
        let root = roots.entry(top.clone()).or_insert_with(|| unique_path(dest, Path::new(&top))).clone();
        let target = if rest.as_os_str().is_empty() { root.clone() } else { root.join(rest) };
        first.get_or_insert(root);

        if e.is_dir {
            tokio::fs::create_dir_all(&target).await?;
            continue;
        }
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let part = target.with_extension(format!("{}seam-part", target.extension().map(|x| format!("{}.", x.to_string_lossy())).unwrap_or_default()));
        let mut recv = net.open_file(&from, FileRequest { offer: offer.id, index: index as u32, offset: 0 }).await?;
        let mut file = tokio::fs::File::create(&part).await?;
        let result: anyhow::Result<()> = async {
            while let Some(chunk) = recv.read_chunk(CHUNK, true).await? {
                file.write_all(&chunk.bytes).await?;
                progress(chunk.bytes.len() as u64);
            }
            file.flush().await?;
            Ok(())
        }
        .await;
        drop(file);
        if let Err(err) = result {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(err);
        }
        tokio::fs::rename(&part, &target).await?;
    }
    first.ok_or_else(|| anyhow::anyhow!("nothing received"))
}

/// Download every file of `offer` into `dest`, keeping its relative path
/// and modification time. Several files at once, since a workspace is
/// mostly small files.
pub async fn download(net: &Net, from: DeviceId, offer: &FileOffer, dest: &Path, progress: Arc<dyn Fn(u64) + Send + Sync>) -> anyhow::Result<()> {
    let limit = Arc::new(tokio::sync::Semaphore::new(16));
    let mut tasks = tokio::task::JoinSet::new();
    for (index, e) in offer.entries.iter().enumerate().filter(|(_, e)| !e.is_dir) {
        let rel = sanitize(&e.rel).ok_or_else(|| anyhow::anyhow!("refused unsafe file name {:?}", e.rel))?;
        let target = dest.join(rel);
        let (net, progress, permit) = (net.clone(), progress.clone(), limit.clone().acquire_owned().await?);
        let (id, modified) = (offer.id, e.modified);
        tasks.spawn(async move {
            let _permit = permit;
            if let Some(parent) = target.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let part = target.with_file_name(format!("{}.seam-part", target.file_name().unwrap().to_string_lossy()));
            let mut recv = net.open_file(&from, FileRequest { offer: id, index: index as u32, offset: 0 }).await?;
            let mut file = tokio::fs::File::create(&part).await?;
            while let Some(chunk) = recv.read_chunk(CHUNK, true).await? {
                file.write_all(&chunk.bytes).await?;
                progress(chunk.bytes.len() as u64);
            }
            file.flush().await?;
            drop(file);
            tokio::fs::rename(&part, &target).await?;
            // FILETIME counts 100 ns steps from 1601.
            if let Some(secs) = modified.checked_sub(116_444_736_000_000_000) {
                let t = std::time::UNIX_EPOCH + std::time::Duration::from_nanos(secs * 100);
                if let Ok(f) = std::fs::File::options().write(true).open(&target) {
                    let _ = f.set_modified(t);
                }
            }
            anyhow::Ok(())
        });
        // Surface the first failure early instead of after every file.
        while let Some(done) = tasks.try_join_next() {
            done??;
        }
    }
    while let Some(done) = tasks.join_next().await {
        done??;
    }
    Ok(())
}

/// Blocking reader over a network file stream, for Explorer's paste/drop.
pub struct NetSource {
    pub net: Net,
    pub rt: tokio::runtime::Handle,
}

struct ChanReader {
    rx: tokio::sync::mpsc::Receiver<std::io::Result<bytes::Bytes>>,
    buf: bytes::Bytes,
}

impl Read for ChanReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.buf.is_empty() {
            match self.rx.blocking_recv() {
                Some(Ok(b)) => self.buf = b,
                Some(Err(e)) => return Err(e),
                None => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len());
        out[..n].copy_from_slice(&self.buf.split_to(n));
        Ok(n)
    }
}

impl FileSource for NetSource {
    fn open(&self, from: DeviceId, offer: u64, index: u32, offset: u64) -> anyhow::Result<Box<dyn Read + Send>> {
        let (tx, rx) = tokio::sync::mpsc::channel(32);
        let net = self.net.clone();
        self.rt.spawn(async move {
            let res: anyhow::Result<()> = async {
                let mut recv = net.open_file(&from, FileRequest { offer, index, offset }).await?;
                while let Some(chunk) = recv.read_chunk(CHUNK, true).await? {
                    if tx.send(Ok(chunk.bytes)).await.is_err() {
                        break;
                    }
                }
                Ok(())
            }
            .await;
            if let Err(e) = res {
                let _ = tx.send(Err(std::io::Error::other(e.to_string()))).await;
            }
        });
        Ok(Box::new(ChanReader { rx, buf: bytes::Bytes::new() }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rejects_escapes() {
        assert_eq!(sanitize("a\\b.txt"), Some(PathBuf::from("a").join("b.txt")));
        assert_eq!(sanitize("..\\evil.exe"), None);
        assert_eq!(sanitize("C:\\Windows\\x"), None);
        assert_eq!(sanitize("a\\\\b"), None);
        assert_eq!(sanitize("CON.txt"), Some(PathBuf::from("_CON.txt")));
        assert_eq!(sanitize("trailing. "), Some(PathBuf::from("trailing")));
    }

    #[test]
    fn walk_and_unique() {
        let dir = std::env::temp_dir().join(format!("seam-test-{}", rand::random::<u32>()));
        std::fs::create_dir_all(dir.join("folder").join("sub")).unwrap();
        std::fs::write(dir.join("folder").join("a.txt"), b"hello").unwrap();
        std::fs::write(dir.join("folder").join("sub").join("b.txt"), b"hi").unwrap();
        let offers = Offers::new();
        let me = uuid::Uuid::from_u128(9);
        let offer = offers.create(&[dir.join("folder")], [me].into()).unwrap();
        let rels: Vec<_> = offer.entries.iter().map(|e| (e.rel.as_str(), e.is_dir, e.size)).collect();
        assert_eq!(rels, vec![("folder", true, 0), ("folder\\a.txt", false, 5), ("folder\\sub", true, 0), ("folder\\sub\\b.txt", false, 2)]);
        assert!(offers.lookup(offer.id, 1, &me).is_some());
        assert!(offers.lookup(offer.id, 1, &uuid::Uuid::from_u128(1)).is_none());
        assert!(offers.lookup(offer.id, 0, &me).is_none());

        assert_eq!(unique_path(&dir, Path::new("folder")), dir.join("folder (2)"));
        std::fs::write(dir.join("x.txt"), b"").unwrap();
        assert_eq!(unique_path(&dir, Path::new("x.txt")), dir.join("x (2).txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
