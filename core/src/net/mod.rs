//! Connections to paired devices.
//!
//! Every device listens on one QUIC endpoint and also dials its paired
//! devices, so whichever side can reach the other wins. If both connect at
//! once, the connection started by the lower device id is kept.

pub mod discovery;
pub mod pairing;
pub mod tls;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use quinn::VarInt;
use tokio::sync::mpsc;

use crate::config::PeerRecord;
use crate::identity::Identity;
use crate::protocol::{
    read_frame, write_frame, DeviceId, DeviceInfo, FileRequest, Msg, ALPN, ALPN_PAIR, DEFAULT_PORT, PROTOCOL_VERSION,
};
use discovery::{Discovery, Found};

const DIAL_TIMEOUT: Duration = Duration::from_millis(2500);
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum NetEvent {
    Connected { info: DeviceInfo, addr: SocketAddr },
    Disconnected { id: DeviceId },
    Msg { from: DeviceId, msg: Msg },
    /// The other device wants to read one of the files we offered.
    FileRequest { from: DeviceId, req: FileRequest, send: quinn::SendStream },
    Paired(PeerRecord),
    /// Someone tried a wrong pairing code. `closed` is true when the window
    /// shut because of too many tries.
    PairingFailed { closed: bool },
    PairingClosed,
    PeerAddr { id: DeviceId, addr: SocketAddr },
    DiscoveryChanged,
    Latency { id: DeviceId, ms: f32 },
    Rejected { reason: String },
}

struct Link {
    conn: quinn::Connection,
    tx: mpsc::UnboundedSender<Msg>,
    initiator: DeviceId,
    info: DeviceInfo,
    generation: u64,
}

struct PairWindow {
    code: String,
    until: Instant,
    failures: u32,
}

struct Shared {
    me: Mutex<DeviceInfo>,
    identity: Identity,
    endpoint: quinn::Endpoint,
    discovery: Arc<Discovery>,
    links: Mutex<HashMap<DeviceId, Link>>,
    dialing: Mutex<HashSet<DeviceId>>,
    paired: Mutex<Vec<PeerRecord>>,
    pairing: Mutex<Option<PairWindow>>,
    events: mpsc::UnboundedSender<NetEvent>,
    generation: AtomicU64,
    epoch: Instant,
}

#[derive(Clone)]
pub struct Net(Arc<Shared>);

#[derive(Clone, Debug, serde::Serialize)]
pub struct LinkInfo {
    pub id: DeviceId,
    pub name: String,
    pub addr: String,
}

impl Net {
    pub async fn start(
        me: DeviceInfo,
        identity: Identity,
        port: u16,
        paired: Vec<PeerRecord>,
        events: mpsc::UnboundedSender<NetEvent>,
    ) -> anyhow::Result<Self> {
        let server = tls::server_config(&identity)?;
        let endpoint = bind(server, port)?;
        let port = endpoint.local_addr()?.port();
        tracing::info!("listening on UDP {port}");

        let ev = events.clone();
        let discovery = Discovery::start(me.id, me.name.clone(), port, move || {
            let _ = ev.send(NetEvent::DiscoveryChanged);
        })?;

        let net = Net(Arc::new(Shared {
            me: Mutex::new(me),
            identity,
            endpoint,
            discovery,
            links: Mutex::default(),
            dialing: Mutex::default(),
            paired: Mutex::new(paired),
            pairing: Mutex::new(None),
            events,
            generation: AtomicU64::new(1),
            epoch: Instant::now(),
        }));

        tokio::spawn(net.clone().accept_loop());
        tokio::spawn(net.clone().dial_loop());
        tokio::spawn(net.clone().ping_loop());
        Ok(net)
    }

    fn emit(&self, ev: NetEvent) {
        let _ = self.0.events.send(ev);
    }

    pub fn me(&self) -> DeviceInfo {
        self.0.me.lock().clone()
    }

    pub fn fingerprint(&self) -> String {
        self.0.identity.fingerprint.clone()
    }

    pub fn port(&self) -> u16 {
        self.0.endpoint.local_addr().map(|a| a.port()).unwrap_or(DEFAULT_PORT)
    }

    pub fn discovered(&self) -> Vec<Found> {
        self.0.discovery.all()
    }

    pub fn connected(&self) -> Vec<LinkInfo> {
        self.0
            .links
            .lock()
            .values()
            .map(|l| LinkInfo { id: l.info.id, name: l.info.name.clone(), addr: l.conn.remote_address().to_string() })
            .collect()
    }

    pub fn peer_info(&self, id: &DeviceId) -> Option<DeviceInfo> {
        self.0.links.lock().get(id).map(|l| l.info.clone())
    }

    pub fn is_connected(&self, id: &DeviceId) -> bool {
        self.0.links.lock().contains_key(id)
    }

    pub fn send(&self, to: &DeviceId, msg: Msg) -> bool {
        match self.0.links.lock().get(to) {
            Some(l) => l.tx.send(msg).is_ok(),
            None => false,
        }
    }

    pub fn broadcast(&self, msg: Msg) {
        for l in self.0.links.lock().values() {
            let _ = l.tx.send(msg.clone());
        }
    }

    /// Update what we tell peers about ourselves (new name or monitors).
    pub fn update_me(&self, f: impl FnOnce(&mut DeviceInfo)) {
        let (name, mons) = {
            let mut me = self.0.me.lock();
            f(&mut me);
            (me.name.clone(), me.monitors.clone())
        };
        self.0.discovery.set_name(name);
        self.broadcast(Msg::Monitors(mons));
    }

    pub fn set_paired(&self, peers: Vec<PeerRecord>) {
        let keep: HashSet<String> = peers.iter().map(|p| p.fingerprint.clone()).collect();
        *self.0.paired.lock() = peers;
        // Drop links to anything no longer paired.
        let mut links = self.0.links.lock();
        links.retain(|_, l| {
            let ok = crate::net::tls::peer_fingerprint(&l.conn).is_some_and(|fp| keep.contains(&fp));
            if !ok {
                l.conn.close(VarInt::from_u32(3), b"unpaired");
            }
            ok
        });
    }

    /// Ask a peer for one of the files it offered.
    pub async fn open_file(&self, from: &DeviceId, req: FileRequest) -> anyhow::Result<quinn::RecvStream> {
        let conn = self.0.links.lock().get(from).map(|l| l.conn.clone()).ok_or_else(|| anyhow::anyhow!("device is not connected"))?;
        let (mut send, recv) = conn.open_bi().await?;
        write_frame(&mut send, &req).await?;
        let _ = send.finish();
        Ok(recv)
    }

    // Pairing ------------------------------------------------------------

    pub fn begin_pairing(&self) -> String {
        let code = pairing::new_code();
        *self.0.pairing.lock() =
            Some(PairWindow { code: code.clone(), until: Instant::now() + Duration::from_secs(pairing::WINDOW_SECS), failures: 0 });
        self.0.discovery.set_pairing(true);
        let net = self.clone();
        let c = code.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(pairing::WINDOW_SECS)).await;
            let still = net.0.pairing.lock().as_ref().is_some_and(|w| w.code == c);
            if still {
                net.end_pairing();
                net.emit(NetEvent::PairingClosed);
            }
        });
        code
    }

    pub fn end_pairing(&self) {
        *self.0.pairing.lock() = None;
        self.0.discovery.set_pairing(false);
    }

    /// Pair with a device that is showing `code`.
    pub async fn pair_with(&self, addrs: Vec<SocketAddr>, code: &str) -> anyhow::Result<PeerRecord> {
        let cfg = tls::client_config(&self.0.identity, ALPN_PAIR)?;
        let mut last_err = anyhow::anyhow!("no address to try");
        for addr in addrs {
            let attempt = async {
                let conn = self.0.endpoint.connect_with(cfg.clone(), addr, "seam")?.await?;
                let their_fp = tls::peer_fingerprint(&conn).ok_or_else(|| anyhow::anyhow!("no certificate"))?;
                let me = self.me();
                let res = pairing::run_client(&conn, code, (me.id, me.name), &self.0.identity.fingerprint, &their_fp).await;
                conn.close(VarInt::from_u32(0), b"done");
                let p = res?;
                Ok::<_, anyhow::Error>(PeerRecord { id: p.id, name: p.name, fingerprint: their_fp, last_addr: Some(addr.to_string()), manual_addr: None })
            };
            match tokio::time::timeout(Duration::from_secs(15), attempt).await {
                Ok(Ok(rec)) => {
                    self.add_paired(rec.clone());
                    return Ok(rec);
                }
                Ok(Err(e)) => {
                    // A wrong code is final; an unreachable address is not.
                    if e.to_string().contains("code") {
                        return Err(e);
                    }
                    last_err = e;
                }
                Err(_) => last_err = anyhow::anyhow!("timed out reaching {addr}"),
            }
        }
        Err(last_err)
    }

    fn add_paired(&self, rec: PeerRecord) {
        let mut p = self.0.paired.lock();
        p.retain(|x| x.id != rec.id);
        p.push(rec);
    }

    // Accepting ----------------------------------------------------------

    async fn accept_loop(self) {
        while let Some(incoming) = self.0.endpoint.accept().await {
            let net = self.clone();
            tokio::spawn(async move {
                match incoming.await {
                    Ok(conn) => {
                        if let Err(e) = net.handle_incoming(conn).await {
                            tracing::debug!("incoming connection ended: {e}");
                        }
                    }
                    Err(e) => tracing::debug!("handshake failed: {e}"),
                }
            });
        }
    }

    async fn handle_incoming(&self, conn: quinn::Connection) -> anyhow::Result<()> {
        let fp = tls::peer_fingerprint(&conn).ok_or_else(|| anyhow::anyhow!("no client certificate"))?;
        let alpn = tls::negotiated_alpn(&conn).unwrap_or_default();

        if alpn == ALPN_PAIR {
            return self.serve_pairing(conn, fp).await;
        }
        anyhow::ensure!(alpn == ALPN, "unknown protocol");

        let Some(rec) = self.0.paired.lock().iter().find(|p| p.fingerprint == fp).cloned() else {
            conn.close(VarInt::from_u32(2), b"not paired");
            anyhow::bail!("rejected unpaired device from {}", conn.remote_address());
        };

        let (mut send, mut recv) = tokio::time::timeout(HELLO_TIMEOUT, conn.accept_bi()).await??;
        let Msg::Hello(info) = tokio::time::timeout(HELLO_TIMEOUT, read_frame::<_, Msg>(&mut recv)).await?? else {
            anyhow::bail!("expected hello");
        };
        if info.id != rec.id {
            conn.close(VarInt::from_u32(2), b"identity mismatch");
            anyhow::bail!("device id does not match its certificate");
        }
        if info.protocol != PROTOCOL_VERSION {
            self.emit(NetEvent::Rejected { reason: format!("{} runs a different version of Seam. Update both devices.", info.name) });
            conn.close(VarInt::from_u32(4), b"version");
            anyhow::bail!("protocol mismatch");
        }
        write_frame(&mut send, &Msg::Hello(self.me())).await?;
        self.register(conn, send, recv, rec.id, info);
        Ok(())
    }

    async fn serve_pairing(&self, conn: quinn::Connection, their_fp: String) -> anyhow::Result<()> {
        let code = {
            let w = self.0.pairing.lock();
            match w.as_ref() {
                Some(w) if Instant::now() < w.until => w.code.clone(),
                _ => {
                    conn.close(VarInt::from_u32(5), b"not pairing");
                    anyhow::bail!("pairing attempt while not pairing");
                }
            }
        };
        let me = self.me();
        let res = pairing::run_server(&conn, &code, (me.id, me.name), &self.0.identity.fingerprint, &their_fp).await;
        let addr = conn.remote_address();
        conn.close(VarInt::from_u32(0), b"done");
        match res {
            Ok(p) => {
                let rec = PeerRecord { id: p.id, name: p.name, fingerprint: their_fp, last_addr: Some(addr.to_string()), manual_addr: None };
                self.add_paired(rec.clone());
                self.end_pairing();
                self.emit(NetEvent::Paired(rec));
            }
            Err(e) => {
                let closed = {
                    let mut w = self.0.pairing.lock();
                    match w.as_mut() {
                        Some(w) => {
                            w.failures += 1;
                            w.failures >= pairing::MAX_FAILURES
                        }
                        None => false,
                    }
                };
                if closed {
                    self.end_pairing();
                }
                self.emit(NetEvent::PairingFailed { closed });
                tracing::info!("pairing attempt from {addr} failed: {e}");
            }
        }
        Ok(())
    }

    // Dialing ------------------------------------------------------------

    async fn dial_loop(self) {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        // The lower device id dials first. The other side holds back for a
        // few seconds so the two rarely open duplicate connections.
        let mut waiting: HashMap<DeviceId, Instant> = HashMap::new();
        loop {
            tick.tick().await;
            let me = self.0.me.lock().id;
            let todo: Vec<PeerRecord> = {
                let links = self.0.links.lock();
                let dialing = self.0.dialing.lock();
                waiting.retain(|id, _| !links.contains_key(id));
                self.0
                    .paired
                    .lock()
                    .iter()
                    .filter(|p| !links.contains_key(&p.id) && !dialing.contains(&p.id))
                    .filter(|p| {
                        me < p.id || p.manual_addr.is_some() || waiting.entry(p.id).or_insert_with(Instant::now).elapsed() > Duration::from_secs(5)
                    })
                    .cloned()
                    .collect()
            };
            for rec in todo {
                self.0.dialing.lock().insert(rec.id);
                let net = self.clone();
                tokio::spawn(async move {
                    net.dial(&rec).await;
                    net.0.dialing.lock().remove(&rec.id);
                });
            }
        }
    }

    fn candidates(&self, rec: &PeerRecord) -> Vec<SocketAddr> {
        let mut out = Vec::new();
        if let Some(m) = &rec.manual_addr {
            if let Some(a) = parse_addr(m) {
                out.push(a);
            }
        }
        out.extend(self.0.discovery.addrs_for(&rec.id));
        if let Some(a) = rec.last_addr.as_deref().and_then(parse_addr) {
            out.push(a);
        }
        out.dedup();
        out
    }

    async fn dial(&self, rec: &PeerRecord) {
        let Ok(cfg) = tls::client_config(&self.0.identity, ALPN) else { return };
        for addr in self.candidates(rec) {
            let attempt = async {
                let conn = self.0.endpoint.connect_with(cfg.clone(), addr, "seam")?.await?;
                if tls::peer_fingerprint(&conn).as_deref() != Some(rec.fingerprint.as_str()) {
                    conn.close(VarInt::from_u32(2), b"identity mismatch");
                    anyhow::bail!("{addr} presented a different certificate");
                }
                let (mut send, mut recv) = conn.open_bi().await?;
                write_frame(&mut send, &Msg::Hello(self.me())).await?;
                let Msg::Hello(info) = tokio::time::timeout(HELLO_TIMEOUT, read_frame::<_, Msg>(&mut recv)).await?? else {
                    anyhow::bail!("expected hello");
                };
                anyhow::ensure!(info.id == rec.id, "device id does not match its certificate");
                Ok((conn, send, recv, info))
            };
            match tokio::time::timeout(DIAL_TIMEOUT + HELLO_TIMEOUT, attempt).await {
                Ok(Ok((conn, send, recv, info))) => {
                    let me = self.me().id;
                    self.emit(NetEvent::PeerAddr { id: rec.id, addr });
                    self.register(conn, send, recv, me, info);
                    return;
                }
                Ok(Err(e)) => tracing::debug!("dial {addr}: {e}"),
                Err(_) => tracing::debug!("dial {addr}: timed out"),
            }
        }
    }

    // Links --------------------------------------------------------------

    fn register(&self, conn: quinn::Connection, send: quinn::SendStream, recv: quinn::RecvStream, initiator: DeviceId, info: DeviceInfo) {
        let id = info.id;
        let generation = self.0.generation.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        {
            let mut links = self.0.links.lock();
            if let Some(old) = links.get(&id) {
                // Same direction means a reconnect: the new one wins. Opposite
                // directions means both dialed at once: lower initiator wins.
                let keep_old = old.initiator != initiator && old.initiator < initiator;
                if keep_old {
                    conn.close(VarInt::from_u32(6), b"duplicate");
                    return;
                }
                old.conn.close(VarInt::from_u32(6), b"duplicate");
            }
            links.insert(id, Link { conn: conn.clone(), tx, initiator, info: info.clone(), generation });
        }
        tracing::info!("connected to {} ({})", info.name, conn.remote_address());
        self.emit(NetEvent::Connected { info, addr: conn.remote_address() });

        tokio::spawn(writer(send, rx));
        tokio::spawn(self.clone().reader(recv, id, generation));
        tokio::spawn(self.clone().file_streams(conn, id));
    }

    fn drop_link(&self, id: DeviceId, generation: u64) {
        let removed = {
            let mut links = self.0.links.lock();
            match links.get(&id) {
                Some(l) if l.generation == generation => links.remove(&id),
                _ => None,
            }
        };
        if let Some(l) = removed {
            l.conn.close(VarInt::from_u32(0), b"bye");
            tracing::info!("disconnected from {}", l.info.name);
            self.emit(NetEvent::Disconnected { id });
        }
    }

    async fn reader(self, mut recv: quinn::RecvStream, id: DeviceId, generation: u64) {
        loop {
            match read_frame::<_, Msg>(&mut recv).await {
                Ok(Msg::Ping(t)) => {
                    self.send(&id, Msg::Pong(t));
                }
                Ok(Msg::Pong(t)) => {
                    let now = self.0.epoch.elapsed().as_micros() as u64;
                    let ms = now.saturating_sub(t) as f32 / 1000.0;
                    self.emit(NetEvent::Latency { id, ms });
                }
                Ok(Msg::Monitors(m)) => {
                    if let Some(l) = self.0.links.lock().get_mut(&id) {
                        l.info.monitors = m.clone();
                    }
                    self.emit(NetEvent::Msg { from: id, msg: Msg::Monitors(m) });
                }
                Ok(msg) => self.emit(NetEvent::Msg { from: id, msg }),
                Err(e) => {
                    tracing::debug!("control stream closed: {e}");
                    break;
                }
            }
        }
        self.drop_link(id, generation);
    }

    async fn file_streams(self, conn: quinn::Connection, from: DeviceId) {
        while let Ok((send, mut recv)) = conn.accept_bi().await {
            let ev = self.0.events.clone();
            tokio::spawn(async move {
                if let Ok(Ok(req)) = tokio::time::timeout(HELLO_TIMEOUT, read_frame::<_, FileRequest>(&mut recv)).await {
                    let _ = ev.send(NetEvent::FileRequest { from, req, send });
                }
            });
        }
    }

    async fn ping_loop(self) {
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tick.tick().await;
            let t = self.0.epoch.elapsed().as_micros() as u64;
            self.broadcast(Msg::Ping(t));
        }
    }

    pub fn shutdown(&self) {
        self.broadcast(Msg::Leave);
        for l in self.0.links.lock().values() {
            l.conn.close(VarInt::from_u32(0), b"shutdown");
        }
        self.0.discovery.shutdown();
        self.0.endpoint.close(VarInt::from_u32(0), b"shutdown");
    }
}

/// Sends queued messages, merging runs of mouse moves so a 1000 Hz mouse
/// never builds a backlog: only the newest position matters.
async fn writer(mut send: quinn::SendStream, mut rx: mpsc::UnboundedReceiver<Msg>) {
    let mut batch: Vec<Msg> = Vec::with_capacity(64);
    while let Some(first) = rx.recv().await {
        batch.push(first);
        while let Ok(m) = rx.try_recv() {
            if matches!(m, Msg::Move { .. }) && matches!(batch.last(), Some(Msg::Move { .. })) {
                *batch.last_mut().unwrap() = m;
            } else {
                batch.push(m);
            }
            if batch.len() >= 256 {
                break;
            }
        }
        let mut buf = Vec::with_capacity(batch.len() * 16);
        for m in batch.drain(..) {
            if write_frame(&mut buf, &m).await.is_err() {
                continue;
            }
        }
        if send.write_all(&buf).await.is_err() {
            break;
        }
    }
    let _ = send.finish();
}

fn bind(server: quinn::ServerConfig, port: u16) -> anyhow::Result<quinn::Endpoint> {
    let tries: &[u16] = if port == 0 { &[0] } else { &[port, port.saturating_add(1), port.saturating_add(2), 0] };
    // SEAM_BIND=127.0.0.1 keeps test instances off the network.
    let ip: std::net::IpAddr = std::env::var("SEAM_BIND").ok().and_then(|s| s.parse().ok()).unwrap_or([0, 0, 0, 0].into());
    for &p in tries {
        match quinn::Endpoint::server(server.clone(), SocketAddr::new(ip, p)) {
            Ok(ep) => return Ok(ep),
            Err(e) => tracing::warn!("UDP {p} unavailable: {e}"),
        }
    }
    anyhow::bail!("could not open a network port")
}

pub fn parse_addr(s: &str) -> Option<SocketAddr> {
    let s = s.trim();
    s.parse::<SocketAddr>().ok().or_else(|| s.parse::<std::net::IpAddr>().ok().map(|ip| SocketAddr::new(ip, DEFAULT_PORT)))
}
