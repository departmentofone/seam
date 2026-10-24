//! Where the clipboard and drag-and-drop code runs.
//!
//! Windows blocks OLE clipboard data and drag-and-drop between an elevated
//! process and Explorer, which runs at normal user level. When Seam runs as
//! administrator (so it can control admin windows), its clipboard thread
//! therefore lives in a small helper process started at normal level, and
//! the two talk over a named pipe. Otherwise it runs in-process as before.
//!
//! File bytes for pasted or dropped virtual files are pulled through the
//! pipe on demand, one chunk per request, so a slow disk on the receiving
//! side never makes the main process buffer a whole file.

use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
use tokio::sync::mpsc;

use super::clipboard::{Clip, ClipCmd, ClipOut};
use super::dataobject::FileSource;
use crate::config::ClipboardSettings;
use crate::net::Net;
use crate::protocol::{read_frame, write_frame, DeviceId, FileRequest};

const CHUNK: u32 = 256 * 1024;

#[derive(Debug, Serialize, Deserialize)]
enum ToHelper {
    Cmd(ClipCmd),
    Settings(ClipboardSettings),
    CancelDrag,
    /// Answer to `Read`. Empty data means end of file.
    Chunk { id: u64, data: Vec<u8>, error: Option<String> },
}

#[derive(Debug, Serialize, Deserialize)]
enum FromHelper {
    Out(ClipOut),
    DragActive(bool),
    Open { id: u64, from: DeviceId, offer: u64, index: u32, offset: u64 },
    Read { id: u64, len: u32 },
    Close { id: u64 },
}

/// The engine's handle on the clipboard thread, wherever it runs.
pub enum OleHost {
    Local { clip: Clip, settings: Arc<RwLock<ClipboardSettings>> },
    Remote(Remote),
}

impl OleHost {
    pub fn local(settings: ClipboardSettings, net: Net, out: mpsc::UnboundedSender<ClipOut>) -> anyhow::Result<Self> {
        let settings = Arc::new(RwLock::new(settings));
        let source = Arc::new(crate::files::NetSource { net, rt: tokio::runtime::Handle::current() });
        let clip = Clip::start(settings.clone(), source, out)?;
        Ok(OleHost::Local { clip, settings })
    }

    pub fn send(&self, cmd: ClipCmd) {
        match self {
            OleHost::Local { clip, .. } => clip.send(cmd),
            OleHost::Remote(r) => r.send(ToHelper::Cmd(cmd)),
        }
    }

    pub fn set_settings(&self, s: ClipboardSettings) {
        match self {
            OleHost::Local { settings, .. } => *settings.write() = s,
            OleHost::Remote(r) => {
                *r.settings.lock() = s.clone();
                r.send(ToHelper::Settings(s));
            }
        }
    }

    pub fn drag_active(&self) -> bool {
        match self {
            OleHost::Local { clip, .. } => clip.drag_active.load(Ordering::Relaxed),
            OleHost::Remote(r) => r.drag_active.load(Ordering::Relaxed),
        }
    }

    pub fn cancel_drag(&self) {
        match self {
            OleHost::Local { clip, .. } => clip.cancel_drag.store(true, Ordering::Relaxed),
            OleHost::Remote(r) => r.send(ToHelper::CancelDrag),
        }
    }

    pub fn is_remote(&self) -> bool {
        matches!(self, OleHost::Remote(_))
    }
}

// ---------------------------------------------------------------------------
// Main process side

pub struct Remote {
    tx: mpsc::UnboundedSender<ToHelper>,
    drag_active: Arc<AtomicBool>,
    settings: Arc<Mutex<ClipboardSettings>>,
}

impl Remote {
    fn send(&self, m: ToHelper) {
        let _ = self.tx.send(m);
    }

    /// Start the helper and keep it running: if it exits, start a new one.
    pub async fn spawn(settings: ClipboardSettings, net: Net, out: mpsc::UnboundedSender<ClipOut>) -> anyhow::Result<OleHost> {
        let (tx, rx) = mpsc::unbounded_channel();
        let drag_active = Arc::new(AtomicBool::new(false));
        let settings = Arc::new(Mutex::new(settings));
        let first = launch_and_connect().await?;
        tokio::spawn(supervise(first, rx, tx.clone(), net, out, drag_active.clone(), settings.clone()));
        Ok(OleHost::Remote(Remote { tx, drag_active, settings }))
    }
}

async fn launch_and_connect() -> anyhow::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    let name = format!(r"\\.\pipe\seam-ole-{}", uuid::Uuid::new_v4().simple());
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let args = format!("--ole-helper {name}");
    if super::is_elevated() {
        let h = super::spawn_unelevated(&exe, &args)?;
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(h);
        }
    } else {
        std::process::Command::new(&exe).args(["--ole-helper", &name]).spawn()?;
    }
    for _ in 0..200 {
        match ClientOptions::new().open(&name) {
            Ok(c) => return Ok(c),
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    anyhow::bail!("the clipboard helper did not start")
}

async fn supervise(
    mut pipe: tokio::net::windows::named_pipe::NamedPipeClient,
    mut rx: mpsc::UnboundedReceiver<ToHelper>,
    tx: mpsc::UnboundedSender<ToHelper>,
    net: Net,
    out: mpsc::UnboundedSender<ClipOut>,
    drag_active: Arc<AtomicBool>,
    settings: Arc<Mutex<ClipboardSettings>>,
) {
    let mut restarts = 0u32;
    loop {
        tracing::info!("clipboard helper connected");
        let _ = tx.send(ToHelper::Settings(settings.lock().clone()));
        let (mut rd, mut wr) = tokio::io::split(pipe);
        let mut streams: HashMap<u64, mpsc::UnboundedSender<u32>> = HashMap::new();
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(m) => if let Err(e) = write_frame(&mut wr, &m).await {
                        tracing::warn!("clipboard helper write failed: {e}");
                        break;
                    },
                    None => return,
                },
                frame = read_frame::<_, FromHelper>(&mut rd) => match frame {
                    Ok(FromHelper::Out(ev)) => { let _ = out.send(ev); }
                    Ok(FromHelper::DragActive(b)) => drag_active.store(b, Ordering::Relaxed),
                    Ok(FromHelper::Open { id, from, offer, index, offset }) => {
                        let (stx, srx) = mpsc::unbounded_channel();
                        streams.insert(id, stx);
                        tokio::spawn(serve_stream(net.clone(), id, FileRequest { offer, index, offset }, from, srx, tx.clone()));
                    }
                    Ok(FromHelper::Read { id, len }) => match streams.get(&id) {
                        Some(s) => { let _ = s.send(len); }
                        None => { let _ = tx.send(ToHelper::Chunk { id, data: Vec::new(), error: Some("stream closed".into()) }); }
                    },
                    Ok(FromHelper::Close { id }) => { streams.remove(&id); }
                    Err(e) => {
                        tracing::warn!("clipboard helper disconnected: {e}");
                        break;
                    }
                },
            }
        }
        drag_active.store(false, Ordering::Relaxed);
        restarts += 1;
        if restarts > 5 {
            tracing::error!("clipboard helper keeps exiting; copying files between PCs is off until Seam restarts");
            return;
        }
        tokio::time::sleep(Duration::from_secs(restarts as u64)).await;
        match launch_and_connect().await {
            Ok(p) => pipe = p,
            Err(e) => {
                tracing::error!("could not restart the clipboard helper: {e}");
                return;
            }
        }
    }
}

async fn serve_stream(
    net: Net,
    id: u64,
    req: FileRequest,
    from: DeviceId,
    mut reqs: mpsc::UnboundedReceiver<u32>,
    tx: mpsc::UnboundedSender<ToHelper>,
) {
    let mut recv = match net.open_file(&from, req).await {
        Ok(r) => Ok(r),
        Err(e) => {
            tracing::warn!("could not open a file on the other device: {e}");
            Err(e.to_string())
        }
    };
    let mut buf = bytes::Bytes::new();
    while let Some(len) = reqs.recv().await {
        let reply = match &mut recv {
            Err(e) => ToHelper::Chunk { id, data: Vec::new(), error: Some(e.clone()) },
            Ok(r) => {
                if buf.is_empty() {
                    match r.read_chunk(CHUNK as usize, true).await {
                        Ok(Some(c)) => buf = c.bytes,
                        Ok(None) => {}
                        Err(e) => {
                            tracing::warn!("file stream from the other device broke: {e}");
                            let _ = tx.send(ToHelper::Chunk { id, data: Vec::new(), error: Some(e.to_string()) });
                            continue;
                        }
                    }
                }
                let n = (len as usize).min(buf.len());
                ToHelper::Chunk { id, data: buf.split_to(n).to_vec(), error: None }
            }
        };
        if tx.send(reply).is_err() {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Helper process side

/// Entry point of `Seam.exe --ole-helper <pipe>`. Runs until the main
/// process goes away.
pub fn run_helper(pipe: &str) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    rt.block_on(helper(pipe))
}

type Pending = Arc<Mutex<HashMap<u64, std::sync::mpsc::Sender<Result<Vec<u8>, String>>>>>;

async fn helper(pipe: &str) -> anyhow::Result<()> {
    let server = ServerOptions::new().first_pipe_instance(true).create(pipe)?;
    tokio::time::timeout(Duration::from_secs(15), server.connect()).await??;
    tracing::info!("clipboard helper running (elevated: {})", super::is_elevated());
    let (mut rd, mut wr) = tokio::io::split(server);

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<FromHelper>();
    tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            if write_frame(&mut wr, &m).await.is_err() {
                break;
            }
        }
    });

    let settings = Arc::new(RwLock::new(ClipboardSettings::default()));
    let pending: Pending = Arc::default();
    let source = Arc::new(PipeSource { out: out_tx.clone(), pending: pending.clone(), next: AtomicU64::new(1) });
    let (clip_tx, mut clip_rx) = mpsc::unbounded_channel::<ClipOut>();
    let clip = Clip::start(settings.clone(), source, clip_tx)?;

    let o = out_tx.clone();
    tokio::spawn(async move {
        while let Some(ev) = clip_rx.recv().await {
            let _ = o.send(FromHelper::Out(ev));
        }
    });

    // Mirror the drag state so the main process can cancel a drag before
    // it releases the mouse button.
    let active = clip.drag_active.clone();
    let o = out_tx.clone();
    tokio::spawn(async move {
        let mut last = false;
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        loop {
            tick.tick().await;
            let now = active.load(Ordering::Relaxed);
            if now != last {
                last = now;
                if o.send(FromHelper::DragActive(now)).is_err() {
                    break;
                }
            }
        }
    });

    loop {
        match read_frame::<_, ToHelper>(&mut rd).await {
            Ok(ToHelper::Cmd(c)) => clip.send(c),
            Ok(ToHelper::Settings(s)) => *settings.write() = s,
            Ok(ToHelper::CancelDrag) => clip.cancel_drag.store(true, Ordering::Relaxed),
            Ok(ToHelper::Chunk { id, data, error }) => {
                if let Some(t) = pending.lock().get(&id) {
                    let _ = t.send(match error {
                        Some(e) => Err(e),
                        None => Ok(data),
                    });
                }
            }
            Err(_) => break,
        }
    }
    tracing::info!("clipboard helper exiting");
    drop(clip);
    Ok(())
}

struct PipeSource {
    out: mpsc::UnboundedSender<FromHelper>,
    pending: Pending,
    next: AtomicU64,
}

impl FileSource for PipeSource {
    fn open(&self, from: DeviceId, offer: u64, index: u32, offset: u64) -> anyhow::Result<Box<dyn Read + Send>> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        self.pending.lock().insert(id, tx);
        self.out.send(FromHelper::Open { id, from, offer, index, offset }).map_err(|_| anyhow::anyhow!("Seam is not running"))?;
        Ok(Box::new(PipeReader { id, rx, out: self.out.clone(), pending: self.pending.clone(), buf: Vec::new(), pos: 0, eof: false }))
    }
}

struct PipeReader {
    id: u64,
    rx: std::sync::mpsc::Receiver<Result<Vec<u8>, String>>,
    out: mpsc::UnboundedSender<FromHelper>,
    pending: Pending,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
}

impl Read for PipeReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.buf.len() {
            if self.eof {
                return Ok(0);
            }
            self.out.send(FromHelper::Read { id: self.id, len: CHUNK }).map_err(|_| std::io::Error::other("Seam is not running"))?;
            match self.rx.recv_timeout(Duration::from_secs(60)) {
                Ok(Ok(data)) => {
                    if data.is_empty() {
                        self.eof = true;
                        return Ok(0);
                    }
                    self.buf = data;
                    self.pos = 0;
                }
                Ok(Err(e)) => return Err(std::io::Error::other(e)),
                Err(_) => return Err(std::io::Error::other("the other device stopped sending")),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Drop for PipeReader {
    fn drop(&mut self) {
        self.pending.lock().remove(&self.id);
        let _ = self.out.send(FromHelper::Close { id: self.id });
    }
}
