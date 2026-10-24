//! Ties the network, input, clipboard and files together, and keeps the UI
//! informed with snapshots of the whole state.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::config::{ClipboardSettings, Config, CrossingSettings, PeerRecord};
use crate::files::{self, Offers};
use crate::identity::{short_fingerprint, Identity};
use crate::layout::{fill_missing, Desk, DeskDevice};
use crate::net::discovery::Found;
use crate::net::{parse_addr, Net, NetEvent};
use crate::protocol::{Button, ClipOffer, DeviceId, DeviceInfo, LayoutDoc, Monitor, Msg, PROTOCOL_VERSION};
use crate::win::clipboard::{ClipCmd, ClipOut, LocalClip};
use crate::win::olehost::{OleHost, Remote};
use crate::win::input::{HookCmd, InjectCmd, Input, InputOut, Shared};

pub type Notify = Arc<dyn Fn(UiEvent) + Send + Sync>;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeView {
    pub id: DeviceId,
    pub name: String,
    pub fingerprint: String,
    pub port: u16,
    pub monitors: Vec<Monitor>,
    pub elevated: bool,
    pub version: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerView {
    pub id: DeviceId,
    pub name: String,
    pub fingerprint: String,
    pub connected: bool,
    pub addr: Option<String>,
    pub latency_ms: Option<f32>,
    pub monitors: Vec<Monitor>,
    pub manual_addr: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Transfer {
    pub id: u64,
    pub incoming: bool,
    pub peer: String,
    pub label: String,
    pub bytes: u64,
    pub total: u64,
    pub done: bool,
    pub error: Option<String>,
    pub path: Option<String>,
}

/// The AI workspace move, as the UI sees it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiView {
    /// idle | scanning | choose | preparing | downloading | ready | applying | done | error
    pub phase: String,
    pub source: Option<DeviceId>,
    pub source_name: Option<String>,
    pub inventory: Option<crate::aiws::Inventory>,
    /// Suggested destination for each project key.
    pub defaults: Vec<(String, String)>,
    pub bytes: u64,
    pub total: u64,
    pub preview: Option<crate::aiws::Report>,
    pub report: Option<crate::aiws::Report>,
    pub error: Option<String>,
    pub claude_running: bool,
    /// Sessions on this PC that the Claude app doesn't list.
    pub unlisted: Option<u32>,
    /// Backup folder of the last change, for Undo.
    pub last_backup: Option<String>,
    pub tools: Vec<(String, String, bool)>,
}

impl Default for AiView {
    fn default() -> Self {
        Self {
            phase: "idle".into(),
            source: None,
            source_name: None,
            inventory: None,
            defaults: Vec::new(),
            bytes: 0,
            total: 0,
            preview: None,
            report: None,
            error: None,
            claude_running: false,
            unlisted: None,
            last_backup: None,
            tools: crate::aiws::TOOLS.iter().map(|(a, b, c)| (a.to_string(), b.to_string(), *c)).collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub me: MeView,
    pub peers: Vec<PeerView>,
    pub discovered: Vec<Found>,
    pub layout: LayoutDoc,
    pub cursor_on: Option<DeviceId>,
    pub controlled_by: Option<DeviceId>,
    pub locked: bool,
    pub public_network: bool,
    pub pairing_code: Option<String>,
    pub pairing_ends_in: Option<u64>,
    pub crossing: CrossingSettings,
    pub clipboard: ClipboardSettings,
    pub sharing: bool,
    pub start_at_login: bool,
    pub run_elevated: bool,
    pub onboarded: bool,
    pub receive_dir: String,
    pub transfers: Vec<Transfer>,
    pub ai: AiView,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UiEvent {
    State(Box<Snapshot>),
    PairResult { ok: bool, message: String },
    Notice { level: String, title: String, body: String },
    /// Cheap update for the tray: where the cursor is.
    Cursor { on: Option<String> },
}

/// Settings the UI may change in one go.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub crossing: Option<CrossingSettings>,
    pub clipboard: Option<ClipboardSettings>,
    pub sharing: Option<bool>,
    pub start_at_login: Option<bool>,
    pub run_elevated: Option<bool>,
    pub onboarded: Option<bool>,
    pub receive_dir: Option<String>,
    pub device_name: Option<String>,
}

#[derive(Debug)]
pub enum Cmd {
    BeginPairing,
    EndPairing,
    PairWith { id: Option<DeviceId>, addr: Option<String>, code: String },
    Unpair(DeviceId),
    SetManualAddr { id: DeviceId, addr: Option<String> },
    SetLayout(LayoutDoc),
    Settings(SettingsPatch),
    SendFiles { to: DeviceId, paths: Vec<PathBuf> },
    GoHome,
    ClearTransfers,
    AiScan { from: DeviceId },
    AiRequest(crate::aiws::Selection),
    AiApply { register_all: bool },
    AiUndo,
    AiReset,
    AiRegisterLocal,
    AiRefresh,
    Shutdown,
}

enum Internal {
    PairDone(Result<PeerRecord, String>),
    ClipFiles(crate::protocol::FileOffer),
    ProbeReady { for_peer: Option<DeviceId>, offer: Option<crate::protocol::FileOffer> },
    Progress { id: u64, bytes: u64 },
    Served { peer: DeviceId, offer: u64, bytes: u64 },
    TransferEnd { id: u64, result: Result<PathBuf, String> },
    AiStatus { running: bool, unlisted: u32 },
    AiDownloaded(Result<(), String>),
    AiPreview(Result<crate::aiws::Report, String>),
    AiApplied(Result<crate::aiws::Report, String>),
    AiUndone(Result<usize, String>),
    AiRegistered(Result<crate::aiws::Report, String>),
}

#[derive(Clone)]
pub struct EngineHandle {
    tx: mpsc::UnboundedSender<Cmd>,
    snapshot: Arc<Mutex<Option<Snapshot>>>,
}

impl EngineHandle {
    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
    pub fn snapshot(&self) -> Option<Snapshot> {
        self.snapshot.lock().clone()
    }
}

struct Engine {
    cfg: Config,
    net: Net,
    input: Input,
    clip: OleHost,
    offers: Arc<Offers>,
    notify: Notify,
    snapshot: Arc<Mutex<Option<Snapshot>>>,
    internal: mpsc::UnboundedSender<Internal>,
    my_monitors: Vec<Monitor>,
    peers_info: HashMap<DeviceId, DeviceInfo>,
    latency: HashMap<DeviceId, f32>,
    cursor_on: Option<DeviceId>,
    controlled_by: Option<DeviceId>,
    locked: bool,
    public_network: bool,
    pairing: Option<(String, Instant)>,
    probe_for: Option<Option<DeviceId>>,
    transfers: Vec<Transfer>,
    /// Outgoing transfer ids by (peer, offer), for serve-side progress.
    serving: HashMap<(DeviceId, u64), u64>,
    next_transfer: u64,
    dirty: bool,
    monitors_dirty: Option<Instant>,
    last_elevation_notice: Option<Instant>,
    ai: AiView,
    ai_job: Option<AiJob>,
}

/// A workspace move in progress on the receiving side.
struct AiJob {
    selection: crate::aiws::Selection,
    manifest: Option<crate::aiws::Manifest>,
    staging: PathBuf,
}

/// Start everything. Must be called inside a tokio runtime.
pub async fn start(notify: Notify) -> anyhow::Result<EngineHandle> {
    crate::win::set_dpi_awareness();
    let mut cfg = Config::load();
    let identity = Identity::load_or_create(&cfg.device_id)?;
    let my_monitors = crate::win::monitors();

    let me = DeviceInfo {
        id: cfg.device_id,
        name: cfg.device_name.clone(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        protocol: PROTOCOL_VERSION,
        monitors: my_monitors.clone(),
    };
    if cfg.layout.get(&me.id).is_none() {
        fill_missing(&mut cfg.layout, &[(me.id, my_monitors.clone())]);
        let _ = cfg.save();
    }

    let (net_tx, mut net_rx) = mpsc::unbounded_channel();
    let net = Net::start(me, identity, cfg.port, cfg.peers.clone(), net_tx).await?;

    let shared = Shared::new(cfg.device_id, cfg.crossing.clone(), cfg.clipboard.drag_files, cfg.sharing);
    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let input = Input::start(shared, input_tx)?;

    // Explorer can't exchange clipboard data or drags with an elevated
    // process, so an elevated Seam keeps that part in a normal-level helper.
    let (clip_tx, mut clip_rx) = mpsc::unbounded_channel();
    let use_helper = crate::win::is_elevated() || std::env::var("SEAM_OLE_HELPER").is_ok_and(|v| v == "1");
    let clip = if use_helper {
        match Remote::spawn(cfg.clipboard.clone(), net.clone(), clip_tx.clone()).await {
            Ok(host) => host,
            Err(e) => {
                tracing::warn!("clipboard helper unavailable ({e}); copying files may not work while running as administrator");
                OleHost::local(cfg.clipboard.clone(), net.clone(), clip_tx)?
            }
        }
    } else {
        OleHost::local(cfg.clipboard.clone(), net.clone(), clip_tx)?
    };
    tracing::info!("clipboard runs {}", if clip.is_remote() { "in the helper process" } else { "in-process" });

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let (int_tx, mut int_rx) = mpsc::unbounded_channel();
    let snapshot = Arc::new(Mutex::new(None));

    let mut engine = Engine {
        cfg,
        net,
        input,
        clip,
        offers: Offers::new(),
        notify,
        snapshot: snapshot.clone(),
        internal: int_tx,
        my_monitors,
        peers_info: HashMap::new(),
        latency: HashMap::new(),
        cursor_on: None,
        controlled_by: None,
        locked: false,
        public_network: tokio::task::spawn_blocking(crate::win::on_public_network).await.unwrap_or(false),
        pairing: None,
        probe_for: None,
        transfers: Vec::new(),
        serving: HashMap::new(),
        next_transfer: 1,
        dirty: true,
        monitors_dirty: None,
        last_elevation_notice: None,
        ai: AiView::default(),
        ai_job: None,
    };
    engine.refresh_ai_status();
    engine.rebuild_desk();
    engine.publish();

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(120));
        let mut slow = tokio::time::interval(Duration::from_secs(15));
        loop {
            tokio::select! {
                Some(ev) = net_rx.recv() => engine.on_net(ev),
                Some(ev) = input_rx.recv() => engine.on_input(ev),
                Some(ev) = clip_rx.recv() => engine.on_clip(ev),
                Some(ev) = int_rx.recv() => engine.on_internal(ev),
                cmd = cmd_rx.recv() => match cmd {
                    Some(Cmd::Shutdown) | None => {
                        engine.net.shutdown();
                        break;
                    }
                    Some(cmd) => engine.on_cmd(cmd),
                },
                _ = tick.tick() => engine.on_tick(),
                _ = slow.tick() => {
                    let public = tokio::task::spawn_blocking(crate::win::on_public_network).await.unwrap_or(false);
                    if public != engine.public_network {
                        engine.public_network = public;
                        engine.dirty = true;
                    }
                    engine.refresh_ai_status();
                }
            }
        }
        tracing::info!("engine stopped");
    });

    Ok(EngineHandle { tx: cmd_tx, snapshot })
}

impl Engine {
    fn ui(&self, ev: UiEvent) {
        (self.notify)(ev);
    }

    fn notice(&self, level: &str, title: &str, body: impl Into<String>) {
        self.ui(UiEvent::Notice { level: level.into(), title: title.into(), body: body.into() });
    }

    fn save(&self) {
        if let Err(e) = self.cfg.save() {
            tracing::error!("could not save settings: {e}");
        }
    }

    fn peer_name(&self, id: &DeviceId) -> String {
        self.peers_info
            .get(id)
            .map(|i| i.name.clone())
            .or_else(|| self.cfg.peer(id).map(|p| p.name.clone()))
            .unwrap_or_else(|| "another device".into())
    }

    fn rebuild_desk(&mut self) {
        let mut devs = vec![(self.cfg.device_id, self.my_monitors.clone())];
        devs.extend(self.peers_info.iter().map(|(id, i)| (*id, i.monitors.clone())));
        if fill_missing(&mut self.cfg.layout, &devs) {
            self.save();
            self.net.broadcast(Msg::Layout(self.cfg.layout.clone()));
        }
        let desk = Desk {
            devices: devs
                .into_iter()
                .filter(|(_, m)| !m.is_empty())
                .filter_map(|(id, monitors)| self.cfg.layout.get(&id).map(|place| DeskDevice { id, monitors, place }))
                .collect(),
        };
        self.input.set_desk(desk);
        self.dirty = true;
    }

    fn on_tick(&mut self) {
        if let Some(t) = self.monitors_dirty {
            if t.elapsed() > Duration::from_millis(400) {
                self.monitors_dirty = None;
                let mons = crate::win::monitors();
                if mons != self.my_monitors {
                    tracing::info!("monitor setup changed");
                    self.my_monitors = mons.clone();
                    self.net.update_me(|me| me.monitors = mons);
                    self.rebuild_desk();
                }
            }
        }
        if let Some((_, until)) = &self.pairing {
            if Instant::now() > *until {
                self.pairing = None;
                self.dirty = true;
            }
        }
        if self.dirty {
            self.publish();
        }
    }

    fn publish(&mut self) {
        self.dirty = false;
        let connected: HashMap<DeviceId, String> = self.net.connected().into_iter().map(|l| (l.id, l.addr)).collect();
        let paired: HashSet<DeviceId> = self.cfg.peers.iter().map(|p| p.id).collect();
        let snap = Snapshot {
            me: MeView {
                id: self.cfg.device_id,
                name: self.cfg.device_name.clone(),
                fingerprint: short_fingerprint(&self.net.fingerprint()),
                port: self.net.port(),
                monitors: self.my_monitors.clone(),
                elevated: self.input.shared.elevated,
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            peers: self
                .cfg
                .peers
                .iter()
                .map(|p| PeerView {
                    id: p.id,
                    name: p.name.clone(),
                    fingerprint: short_fingerprint(&p.fingerprint),
                    connected: connected.contains_key(&p.id),
                    addr: connected.get(&p.id).cloned().or_else(|| p.last_addr.clone()),
                    latency_ms: self.latency.get(&p.id).copied().filter(|_| connected.contains_key(&p.id)),
                    monitors: self.peers_info.get(&p.id).map(|i| i.monitors.clone()).unwrap_or_default(),
                    manual_addr: p.manual_addr.clone(),
                })
                .collect(),
            discovered: self.net.discovered().into_iter().filter(|f| !paired.contains(&f.id)).collect(),
            layout: self.cfg.layout.clone(),
            cursor_on: self.cursor_on,
            controlled_by: self.controlled_by,
            locked: self.locked,
            public_network: self.public_network,
            pairing_code: self.pairing.as_ref().map(|(c, _)| c.clone()),
            pairing_ends_in: self.pairing.as_ref().map(|(_, t)| t.saturating_duration_since(Instant::now()).as_secs()),
            crossing: self.cfg.crossing.clone(),
            clipboard: self.cfg.clipboard.clone(),
            sharing: self.cfg.sharing,
            start_at_login: self.cfg.start_at_login,
            run_elevated: self.cfg.run_elevated,
            onboarded: self.cfg.onboarded,
            receive_dir: self.cfg.receive_dir.clone(),
            transfers: self.transfers.clone(),
            ai: self.ai.clone(),
        };
        *self.snapshot.lock() = Some(snap.clone());
        self.ui(UiEvent::State(Box::new(snap)));
    }

    // Network -----------------------------------------------------------

    fn on_net(&mut self, ev: NetEvent) {
        match ev {
            NetEvent::Connected { info, .. } => {
                let id = info.id;
                if let Some(rec) = self.cfg.peer_mut(&id) {
                    if rec.name != info.name {
                        rec.name = info.name.clone();
                        self.save();
                    }
                }
                self.peers_info.insert(id, info);
                self.rebuild_desk();
                self.net.send(&id, Msg::Layout(self.cfg.layout.clone()));
            }
            NetEvent::Disconnected { id } => {
                self.peers_info.remove(&id);
                self.latency.remove(&id);
                self.input.hook(HookCmd::PeerGone(id));
                if self.controlled_by == Some(id) {
                    self.end_controlled();
                }
                self.rebuild_desk();
            }
            NetEvent::Msg { from, msg } => self.on_msg(from, msg),
            NetEvent::FileRequest { from, req, send } => {
                let offers = self.offers.clone();
                let int = self.internal.clone();
                let offer = req.offer;
                tokio::spawn(async move {
                    let int2 = int.clone();
                    let progress = move |n| {
                        let _ = int2.send(Internal::Served { peer: from, offer, bytes: n });
                    };
                    if let Err(e) = files::serve(&offers, from, req, send, progress).await {
                        tracing::warn!("sending a file stopped: {e}");
                    }
                });
            }
            NetEvent::Paired(rec) => self.add_peer(rec),
            NetEvent::PairingFailed { closed } => {
                if closed {
                    self.pairing = None;
                    self.dirty = true;
                }
                let msg = if closed {
                    "Too many wrong codes. Pairing stopped; start again for a new code."
                } else {
                    "Someone entered a wrong code."
                };
                self.ui(UiEvent::PairResult { ok: false, message: msg.into() });
            }
            NetEvent::PairingClosed => {
                self.pairing = None;
                self.dirty = true;
            }
            NetEvent::PeerAddr { id, addr } => {
                let a = addr.to_string();
                if let Some(rec) = self.cfg.peer_mut(&id) {
                    if rec.last_addr.as_deref() != Some(&a) {
                        rec.last_addr = Some(a);
                        self.save();
                    }
                }
            }
            NetEvent::DiscoveryChanged => self.dirty = true,
            NetEvent::Latency { id, ms } => {
                self.latency.insert(id, ms);
                self.dirty = true;
            }
            NetEvent::Rejected { reason } => self.notice("warn", "Could not connect", reason),
        }
    }

    fn add_peer(&mut self, rec: PeerRecord) {
        let name = rec.name.clone();
        self.cfg.peers.retain(|p| p.id != rec.id);
        self.cfg.peers.push(rec);
        self.save();
        self.net.set_paired(self.cfg.peers.clone());
        self.pairing = None;
        self.dirty = true;
        self.ui(UiEvent::PairResult { ok: true, message: format!("Paired with {name}") });
    }

    fn end_controlled(&mut self) {
        // A drag that came from elsewhere must be cancelled before the button
        // is released, or releasing it would drop the files where the cursor
        // happens to be. The helper needs a moment to see the cancel.
        if self.clip.drag_active() {
            self.clip.cancel_drag();
            if self.clip.is_remote() {
                let inject = self.input.injector();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    let _ = inject.send(InjectCmd::Leave);
                });
                return;
            }
        }
        self.input.inject(InjectCmd::Leave);
    }

    fn on_msg(&mut self, from: DeviceId, msg: Msg) {
        match msg {
            Msg::Hello(_) | Msg::Ping(_) | Msg::Pong(_) => {}
            Msg::Monitors(m) => {
                if let Some(i) = self.peers_info.get_mut(&from) {
                    i.monitors = m;
                }
                self.rebuild_desk();
            }
            Msg::Layout(doc) => {
                let ours = &self.cfg.layout;
                let adopt = doc.revision > ours.revision || (doc.revision == ours.revision && doc != *ours && from < self.cfg.device_id);
                if adopt {
                    self.cfg.layout = doc;
                    self.save();
                    self.rebuild_desk();
                } else if doc.revision < ours.revision {
                    self.net.send(&from, Msg::Layout(ours.clone()));
                }
            }
            Msg::Enter { x, y, buttons } => {
                if !self.cfg.sharing {
                    self.net.send(&from, Msg::TakeBack);
                    return;
                }
                self.input.inject(InjectCmd::Enter { by: from, x, y, buttons });
            }
            Msg::Leave => self.end_controlled(),
            Msg::Move { x, y } => self.input.inject(InjectCmd::Move { x, y }),
            Msg::Button { button, down, x, y } => self.input.inject(InjectCmd::Button { button, down, x, y }),
            Msg::Wheel { horizontal, delta } => self.input.inject(InjectCmd::Wheel { horizontal, delta }),
            Msg::Key { scan, vk, extended, down } => self.input.inject(InjectCmd::Key { scan, vk, extended, down }),
            Msg::TakeBack => self.input.hook(HookCmd::TakeBack { from }),
            Msg::Clipboard(mut offer) => {
                let s = self.cfg.clipboard.clone();
                if !s.text {
                    offer.text = None;
                    offer.html = None;
                }
                if !s.images {
                    offer.png = None;
                }
                if !s.files {
                    offer.files = None;
                }
                if offer.text.is_some() || offer.png.is_some() || offer.files.is_some() {
                    self.clip.send(ClipCmd::Set { from, offer });
                }
            }
            Msg::DragIn { offer, x, y } => {
                if self.cfg.clipboard.drag_files {
                    self.clip.send(ClipCmd::DragIn { from, offer, x, y });
                }
            }
            Msg::DragProbe { x, y } => {
                if self.cfg.clipboard.drag_files {
                    self.probe_for = Some(Some(from));
                    self.clip.send(ClipCmd::Probe { x, y });
                } else {
                    self.net.send(&from, Msg::DragProbeResult { offer: None });
                }
            }
            Msg::DragProbeResult { offer } => self.input.hook(HookCmd::RemoteDrag { from, offer }),
            Msg::DragDone { .. } => {}
            Msg::SendFiles { offer } => self.receive(from, offer),
            Msg::Bye => {
                let name = self.peer_name(&from);
                self.forget(from);
                self.notice("info", "Device removed", format!("{name} unpaired this device."));
            }

            // This PC is the one the workspace comes from.
            Msg::AiScan => {
                let net = self.net.clone();
                tokio::task::spawn_blocking(move || {
                    net.send(&from, Msg::AiInventory(crate::aiws::claude::scan()));
                });
            }
            Msg::AiRequest(sel) => {
                let net = self.net.clone();
                let offers = self.offers.clone();
                tokio::task::spawn_blocking(move || {
                    let scratch = crate::config::data_dir().join("ai-out").join(uuid::Uuid::new_v4().simple().to_string());
                    let res = crate::aiws::claude::collect(&sel, &scratch)
                        .and_then(|(files, manifest)| Ok((offers.create_explicit(files, [from].into(), "AI workspace")?, manifest)));
                    match res {
                        Ok((offer, manifest)) => net.send(&from, Msg::AiOffer { offer, manifest }),
                        Err(e) => net.send(&from, Msg::AiError(e.to_string())),
                    };
                });
            }

            // This PC is the one receiving it.
            Msg::AiInventory(inv) => {
                if self.ai.source == Some(from) && self.ai.phase == "scanning" {
                    self.ai.defaults = ai_defaults(&inv);
                    self.ai.inventory = Some(inv);
                    self.ai.phase = "choose".into();
                    self.dirty = true;
                }
            }
            Msg::AiOffer { offer, manifest } => {
                let Some(job) = self.ai_job.as_mut().filter(|_| self.ai.source == Some(from) && self.ai.phase == "preparing") else { return };
                job.manifest = Some(manifest);
                self.ai.phase = "downloading".into();
                self.ai.bytes = 0;
                self.ai.total = offer.entries.iter().map(|e| e.size).sum();
                self.dirty = true;
                let staging = job.staging.clone();
                let (net, int) = (self.net.clone(), self.internal.clone());
                tokio::spawn(async move {
                    let _ = tokio::fs::remove_dir_all(&staging).await;
                    let i2 = int.clone();
                    let progress: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |n| {
                        let _ = i2.send(Internal::Progress { id: 0, bytes: n });
                    });
                    let res = files::download(&net, from, &offer, &staging, progress).await;
                    let _ = int.send(Internal::AiDownloaded(res.map_err(|e| e.to_string())));
                });
            }
            Msg::AiError(e) => {
                if self.ai.source == Some(from) {
                    self.ai.phase = "error".into();
                    self.ai.error = Some(e);
                    self.dirty = true;
                }
            }
        }
    }

    fn receive(&mut self, from: DeviceId, offer: crate::protocol::FileOffer) {
        let id = self.next_transfer;
        self.next_transfer += 1;
        let files = offer.entries.iter().filter(|e| !e.is_dir).count();
        let total = offer.entries.iter().map(|e| e.size).sum();
        let label = match offer.entries.first() {
            Some(e) if offer.entries.iter().filter(|x| !x.rel.contains('\\')).count() == 1 => e.rel.clone(),
            _ => format!("{files} files"),
        };
        self.transfers.push(Transfer {
            id,
            incoming: true,
            peer: self.peer_name(&from),
            label,
            bytes: 0,
            total,
            done: false,
            error: None,
            path: None,
        });
        self.dirty = true;
        let net = self.net.clone();
        let dest = PathBuf::from(&self.cfg.receive_dir);
        let int = self.internal.clone();
        tokio::spawn(async move {
            let int2 = int.clone();
            let res = files::receive(&net, from, &offer, &dest, move |n| {
                let _ = int2.send(Internal::Progress { id, bytes: n });
            })
            .await;
            let _ = int.send(Internal::TransferEnd { id, result: res.map_err(|e| e.to_string()) });
        });
    }

    fn forget(&mut self, id: DeviceId) {
        self.cfg.peers.retain(|p| p.id != id);
        self.cfg.layout.placements.retain(|(d, _)| *d != id);
        self.cfg.layout.revision += 1;
        self.save();
        self.net.set_paired(self.cfg.peers.clone());
        self.peers_info.remove(&id);
        self.input.hook(HookCmd::PeerGone(id));
        self.rebuild_desk();
    }

    // Input --------------------------------------------------------------

    fn on_input(&mut self, ev: InputOut) {
        match ev {
            InputOut::Send { to, msg } => {
                self.net.send(&to, msg);
            }
            InputOut::Active(dev) => {
                self.cursor_on = dev;
                let name = dev.map(|d| self.peer_name(&d));
                self.ui(UiEvent::Cursor { on: name });
                self.dirty = true;
            }
            InputOut::ControlledBy(dev) => {
                self.controlled_by = dev;
                self.dirty = true;
            }
            InputOut::Locked(l) => {
                self.locked = l;
                self.dirty = true;
                let body = if l { "The cursor stays on this device until you press the key again." } else { "The cursor moves between devices again." };
                self.notice("info", if l { "Cursor locked" } else { "Cursor unlocked" }, body);
            }
            InputOut::MonitorsChanged => self.monitors_dirty = Some(Instant::now()),
            InputOut::ProbeLocalDrag { x, y } => {
                self.probe_for = Some(None);
                self.clip.send(ClipCmd::Probe { x, y });
            }
            InputOut::StartDragIn { from, offer, x, y } => self.clip.send(ClipCmd::DragIn { from, offer, x, y }),
            InputOut::PortalAt { x, y } => self.clip.send(ClipCmd::PortalAt { x, y }),
            InputOut::HidePortal => self.clip.send(ClipCmd::HidePortal),
            InputOut::ElevatedForeground => {
                let recent = self.last_elevation_notice.is_some_and(|t| t.elapsed() < Duration::from_secs(120));
                if !recent {
                    self.last_elevation_notice = Some(Instant::now());
                    self.notice(
                        "warn",
                        "An admin window has focus",
                        "Windows blocks Seam from controlling apps running as administrator. Turn on \"Run as administrator\" in Settings on this device.",
                    );
                }
            }
        }
    }

    // Clipboard ----------------------------------------------------------

    fn on_clip(&mut self, ev: ClipOut) {
        match ev {
            ClipOut::Changed(local) => self.share_clipboard(local),
            ClipOut::Probe(paths) => {
                let Some(for_peer) = self.probe_for.take() else { return };
                let Some(paths) = paths else {
                    let _ = self.internal.send(Internal::ProbeReady { for_peer, offer: None });
                    return;
                };
                let offers = self.offers.clone();
                let peers: HashSet<DeviceId> = self.peers_info.keys().copied().collect();
                let int = self.internal.clone();
                tokio::task::spawn_blocking(move || {
                    let offer = offers.create(&paths, peers).ok();
                    let _ = int.send(Internal::ProbeReady { for_peer, offer });
                });
            }
            ClipOut::PressForDrag { x, y } => self.input.inject(InjectCmd::Local { button: Button::Left, down: true, x, y }),
            ClipOut::DragInEnded { dropped } => tracing::debug!("drag ended, dropped: {dropped}"),
        }
    }

    fn share_clipboard(&mut self, local: LocalClip) {
        if self.peers_info.is_empty() {
            return;
        }
        if let Some(paths) = local.files {
            let offers = self.offers.clone();
            let peers: HashSet<DeviceId> = self.peers_info.keys().copied().collect();
            let int = self.internal.clone();
            tokio::task::spawn_blocking(move || match offers.create(&paths, peers) {
                Ok(offer) => {
                    let _ = int.send(Internal::ClipFiles(offer));
                }
                Err(e) => tracing::warn!("not sharing copied files: {e}"),
            });
            return;
        }
        let offer = ClipOffer { id: 0, text: local.text, html: local.html, png: local.png, files: None };
        self.net.broadcast(Msg::Clipboard(offer));
    }

    fn on_internal(&mut self, ev: Internal) {
        match ev {
            Internal::PairDone(Ok(rec)) => self.add_peer(rec),
            Internal::PairDone(Err(e)) => self.ui(UiEvent::PairResult { ok: false, message: e }),
            Internal::ClipFiles(offer) => {
                self.net.broadcast(Msg::Clipboard(ClipOffer { id: offer.id, files: Some(offer), ..Default::default() }));
            }
            Internal::ProbeReady { for_peer: Some(peer), offer } => {
                self.net.send(&peer, Msg::DragProbeResult { offer });
            }
            Internal::ProbeReady { for_peer: None, offer } => {
                if offer.is_none() {
                    self.clip.send(ClipCmd::HidePortal);
                }
                self.input.hook(HookCmd::LocalDrag(offer));
            }
            Internal::Progress { id: 0, bytes } => {
                self.ai.bytes += bytes;
                self.dirty = true;
            }
            Internal::AiStatus { running, unlisted } => {
                if self.ai.claude_running != running || self.ai.unlisted != Some(unlisted) {
                    self.ai.claude_running = running;
                    self.ai.unlisted = Some(unlisted);
                    self.dirty = true;
                }
            }
            Internal::AiDownloaded(res) => match res {
                Ok(()) => self.ai_plan(),
                Err(e) => self.ai_fail(format!("The copy stopped: {e}")),
            },
            Internal::AiPreview(res) => match res {
                Ok(r) => {
                    self.ai.preview = Some(r);
                    self.ai.phase = "ready".into();
                    self.dirty = true;
                    self.refresh_ai_status();
                }
                Err(e) => self.ai_fail(format!("Couldn't plan the import: {e}")),
            },
            Internal::AiApplied(res) => match res {
                Ok(r) => {
                    self.ai.last_backup = r.backup.clone().or(self.ai.last_backup.take());
                    let sessions = r.sessions_registered;
                    self.ai.report = Some(r);
                    self.ai.phase = "done".into();
                    self.dirty = true;
                    if let Some(job) = self.ai_job.take() {
                        let _ = std::fs::remove_dir_all(&job.staging);
                    }
                    self.notice("ok", "Workspace moved", format!("Open Claude to pick up where you left off. {sessions} sessions were added to its list."));
                    self.refresh_ai_status();
                }
                Err(e) => self.ai_fail(format!("The import stopped: {e}. Anything already changed can be undone.")),
            },
            Internal::AiRegistered(res) => {
                match res {
                    Ok(r) if r.sessions_registered > 0 => {
                        self.ai.last_backup = r.backup.clone();
                        self.notice("ok", "Sessions added", format!("{} sessions now show in the Claude app. Open it to see them.", r.sessions_registered));
                    }
                    Ok(r) => self.notice("info", "Nothing to add", r.notes.first().cloned().unwrap_or_else(|| "Every session is already in the Claude app's list.".into())),
                    Err(e) => self.notice("error", "Couldn't add sessions", e),
                }
                self.dirty = true;
                self.refresh_ai_status();
            }
            Internal::AiUndone(res) => {
                match res {
                    Ok(n) => {
                        self.ai.last_backup = None;
                        self.notice("ok", "Undone", format!("{n} files were put back the way they were."));
                    }
                    Err(e) => self.notice("error", "Couldn't undo", e),
                }
                self.dirty = true;
                self.refresh_ai_status();
            }
            Internal::Progress { id, bytes } => {
                if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
                    t.bytes += bytes;
                    self.dirty = true;
                }
            }
            Internal::Served { peer, offer, bytes } => {
                let tid = match self.serving.get(&(peer, offer)) {
                    Some(t) => *t,
                    None => {
                        let Some((label, total)) = self.offers.describe(offer) else { return };
                        let id = self.next_transfer;
                        self.next_transfer += 1;
                        self.serving.insert((peer, offer), id);
                        self.transfers.push(Transfer {
                            id,
                            incoming: false,
                            peer: self.peer_name(&peer),
                            label,
                            bytes: 0,
                            total,
                            done: false,
                            error: None,
                            path: None,
                        });
                        id
                    }
                };
                if let Some(t) = self.transfers.iter_mut().find(|t| t.id == tid) {
                    t.bytes += bytes;
                    t.done = t.bytes >= t.total;
                    self.dirty = true;
                }
            }
            Internal::TransferEnd { id, result } => {
                let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) else { return };
                t.done = true;
                match result {
                    Ok(path) => {
                        t.bytes = t.total;
                        t.path = Some(path.to_string_lossy().into_owned());
                        let (peer, label) = (t.peer.clone(), t.label.clone());
                        self.notice("ok", "Files received", format!("{label} from {peer}"));
                    }
                    Err(e) => {
                        t.error = Some(e.clone());
                        self.notice("error", "Transfer failed", e);
                    }
                }
                self.dirty = true;
            }
        }
    }

    // Commands from the UI ----------------------------------------------

    fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::BeginPairing => {
                let code = self.net.begin_pairing();
                self.pairing = Some((code, Instant::now() + Duration::from_secs(crate::net::pairing::WINDOW_SECS)));
                self.dirty = true;
            }
            Cmd::EndPairing => {
                self.net.end_pairing();
                self.pairing = None;
                self.dirty = true;
            }
            Cmd::PairWith { id, addr, code } => {
                let mut addrs = Vec::new();
                if let Some(a) = addr.as_deref().and_then(parse_addr) {
                    addrs.push(a);
                }
                if let Some(id) = id {
                    addrs.extend(self.net.discovered().into_iter().filter(|f| f.id == id).flat_map(|f| f.addrs));
                }
                if addrs.is_empty() {
                    self.ui(UiEvent::PairResult { ok: false, message: "Enter an address like 192.168.1.20".into() });
                    return;
                }
                let net = self.net.clone();
                let int = self.internal.clone();
                let code: String = code.chars().filter(|c| c.is_ascii_digit()).collect();
                tokio::spawn(async move {
                    let res = net.pair_with(addrs, &code).await.map_err(|e| friendly_pair_error(&e.to_string()));
                    let _ = int.send(Internal::PairDone(res));
                });
            }
            Cmd::Unpair(id) => {
                self.net.send(&id, Msg::Bye);
                let net = self.net.clone();
                // Let the goodbye leave before the link closes.
                let peers = {
                    let mut p = self.cfg.peers.clone();
                    p.retain(|x| x.id != id);
                    p
                };
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    net.set_paired(peers);
                });
                self.cfg.peers.retain(|p| p.id != id);
                self.cfg.layout.placements.retain(|(d, _)| *d != id);
                self.cfg.layout.revision += 1;
                self.save();
                self.peers_info.remove(&id);
                self.input.hook(HookCmd::PeerGone(id));
                self.rebuild_desk();
            }
            Cmd::SetManualAddr { id, addr } => {
                if let Some(rec) = self.cfg.peer_mut(&id) {
                    rec.manual_addr = addr.filter(|a| !a.trim().is_empty());
                    self.save();
                    self.net.set_paired(self.cfg.peers.clone());
                    self.dirty = true;
                }
            }
            Cmd::SetLayout(mut doc) => {
                doc.revision = self.cfg.layout.revision.max(doc.revision) + 1;
                self.cfg.layout = doc;
                self.save();
                self.net.broadcast(Msg::Layout(self.cfg.layout.clone()));
                self.rebuild_desk();
            }
            Cmd::Settings(p) => self.apply_settings(p),
            Cmd::SendFiles { to, paths } => {
                if !self.net.is_connected(&to) {
                    self.notice("warn", "Not connected", format!("{} is offline.", self.peer_name(&to)));
                    return;
                }
                let offers = self.offers.clone();
                let net = self.net.clone();
                tokio::task::spawn_blocking(move || match offers.create(&paths, [to].into()) {
                    Ok(offer) => {
                        net.send(&to, Msg::SendFiles { offer });
                    }
                    Err(e) => tracing::warn!("could not send files: {e}"),
                });
            }
            Cmd::GoHome => self.input.hook(HookCmd::GoHome),
            Cmd::AiScan { from } => {
                if !self.net.is_connected(&from) {
                    self.notice("warn", "Not connected", format!("{} is offline.", self.peer_name(&from)));
                    return;
                }
                self.ai = AiView { phase: "scanning".into(), source: Some(from), source_name: Some(self.peer_name(&from)), ..AiView::default() };
                self.ai_job = None;
                self.net.send(&from, Msg::AiScan);
                self.refresh_ai_status();
                self.dirty = true;
            }
            Cmd::AiRequest(sel) => {
                let Some(from) = self.ai.source.filter(|_| self.ai.phase == "choose") else { return };
                let staging = std::env::var_os("LOCALAPPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(std::env::temp_dir)
                    .join("Seam")
                    .join("ai-staging")
                    .join(uuid::Uuid::new_v4().simple().to_string());
                self.ai_job = Some(AiJob { selection: sel.clone(), manifest: None, staging });
                self.ai.phase = "preparing".into();
                self.dirty = true;
                self.net.send(&from, Msg::AiRequest(sel));
            }
            Cmd::AiApply { register_all } => {
                let Some(job) = self.ai_job.as_ref().filter(|_| self.ai.phase == "ready") else { return };
                if crate::aiws::claude_running() {
                    self.ai.claude_running = true;
                    self.dirty = true;
                    self.notice("warn", "Claude is still open", "Quit the Claude app (also from the tray) and close any claude terminals, then press Move again.");
                    return;
                }
                let (staging, sel) = (job.staging.clone(), job.selection.clone());
                let Some(manifest) = job.manifest.clone() else { return };
                self.ai.phase = "applying".into();
                self.dirty = true;
                let int = self.internal.clone();
                tokio::task::spawn_blocking(move || {
                    let res = crate::aiws::claude::apply(&staging, &manifest, &sel, register_all, false);
                    let _ = int.send(Internal::AiApplied(res.map_err(|e| e.to_string())));
                });
            }
            Cmd::AiUndo => {
                let Some(dir) = self.ai.last_backup.clone() else { return };
                if crate::aiws::claude_running() {
                    self.notice("warn", "Claude is still open", "Quit Claude first so it doesn't write over the restored files.");
                    return;
                }
                let int = self.internal.clone();
                tokio::task::spawn_blocking(move || {
                    let res = crate::aiws::journal::undo(std::path::Path::new(&dir));
                    let _ = int.send(Internal::AiUndone(res.map_err(|e| e.to_string())));
                });
            }
            Cmd::AiReset => {
                if let Some(job) = self.ai_job.take() {
                    let _ = std::fs::remove_dir_all(&job.staging);
                }
                let keep = (self.ai.claude_running, self.ai.unlisted, self.ai.last_backup.clone());
                self.ai = AiView { claude_running: keep.0, unlisted: keep.1, last_backup: keep.2, ..AiView::default() };
                self.dirty = true;
            }
            Cmd::AiRegisterLocal => {
                if crate::aiws::claude_running() {
                    self.notice("warn", "Claude is still open", "Quit the Claude app first, then press the button again.");
                    return;
                }
                let int = self.internal.clone();
                tokio::task::spawn_blocking(move || {
                    let res = crate::aiws::claude::register_all_local();
                    let _ = int.send(Internal::AiRegistered(res.map_err(|e| e.to_string())));
                });
            }
            Cmd::AiRefresh => self.refresh_ai_status(),
            Cmd::ClearTransfers => {
                self.transfers.retain(|t| !t.done);
                self.dirty = true;
            }
            Cmd::Shutdown => {}
        }
    }

    fn refresh_ai_status(&self) {
        let int = self.internal.clone();
        tokio::task::spawn_blocking(move || {
            let running = crate::aiws::claude_running();
            let unlisted = crate::aiws::claude::unlisted_count();
            let _ = int.send(Internal::AiStatus { running, unlisted });
        });
    }

    /// Work out what the import would do, without writing anything.
    fn ai_plan(&mut self) {
        let Some(job) = &self.ai_job else { return };
        let Some(manifest) = job.manifest.clone() else { return };
        let (staging, sel) = (job.staging.clone(), job.selection.clone());
        let int = self.internal.clone();
        tokio::task::spawn_blocking(move || {
            let res = crate::aiws::claude::apply(&staging, &manifest, &sel, true, true);
            let _ = int.send(Internal::AiPreview(res.map_err(|e| e.to_string())));
        });
    }

    fn ai_fail(&mut self, msg: String) {
        self.ai.phase = "error".into();
        self.ai.error = Some(msg);
        self.dirty = true;
    }

    fn apply_settings(&mut self, p: SettingsPatch) {
        if let Some(c) = p.crossing {
            *self.input.shared.crossing.write() = c.clone();
            self.cfg.crossing = c;
        }
        if let Some(c) = p.clipboard {
            self.input.shared.drag_files.store(c.drag_files, Ordering::Relaxed);
            self.clip.set_settings(c.clone());
            self.cfg.clipboard = c;
        }
        if let Some(s) = p.sharing {
            self.cfg.sharing = s;
            self.input.shared.enabled.store(s, Ordering::Relaxed);
            if !s {
                self.input.hook(HookCmd::GoHome);
                if let Some(by) = self.controlled_by {
                    self.net.send(&by, Msg::TakeBack);
                    self.end_controlled();
                }
            }
        }
        if let Some(v) = p.start_at_login {
            self.cfg.start_at_login = v;
        }
        if let Some(v) = p.run_elevated {
            self.cfg.run_elevated = v;
        }
        if let Some(v) = p.onboarded {
            self.cfg.onboarded = v;
        }
        if let Some(v) = p.receive_dir {
            self.cfg.receive_dir = v;
        }
        if let Some(name) = p.device_name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
            self.cfg.device_name = name.clone();
            self.net.update_me(|me| me.name = name);
        }
        self.save();
        self.dirty = true;
    }
}

/// Suggested destinations. A project inside another one follows its parent.
fn ai_defaults(inv: &crate::aiws::Inventory) -> Vec<(String, String)> {
    use crate::aiws::rewrite::{is_under, normalize};
    let cwds: Vec<&str> = inv.projects.iter().filter_map(|p| p.cwd.as_deref()).collect();
    inv.projects
        .iter()
        .filter_map(|p| {
            let cwd = p.cwd.as_deref()?;
            let parent = cwds.iter().filter(|o| **o != cwd && is_under(cwd, o)).min_by_key(|o| o.len());
            let target = match parent {
                Some(par) => format!("{}{}", crate::aiws::default_target(par, inv), &normalize(cwd)[normalize(par).len()..]),
                None => crate::aiws::default_target(cwd, inv),
            };
            Some((p.key.clone(), target))
        })
        .collect()
}

fn friendly_pair_error(e: &str) -> String {
    if e.contains("code") {
        "That code didn't match. Check the number on the other device and try again.".into()
    } else if e.contains("not pairing") || e.contains("closed") {
        "The other device isn't showing a pairing code right now.".into()
    } else if e.contains("timed out") {
        "Couldn't reach that device. Make sure both are on the same network and Seam is allowed through the firewall.".into()
    } else {
        format!("Pairing failed: {e}")
    }
}
