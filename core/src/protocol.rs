//! Wire protocol shared by every Seam device.
//!
//! Each connection carries one long-lived control stream of length-prefixed
//! postcard frames (`Msg`), plus short-lived streams for file contents.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u16 = 2;
pub const ALPN: &[u8] = b"seam/1";
pub const ALPN_PAIR: &[u8] = b"seam-pair/1";
pub const DEFAULT_PORT: u16 = 47420;
pub const SERVICE_TYPE: &str = "_seam._udp.local.";

/// Largest control frame we accept. Clipboard images are the biggest thing
/// that rides the control stream.
pub const MAX_FRAME: usize = 48 * 1024 * 1024;

pub type DeviceId = Uuid;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Monitor {
    /// Stable-ish identifier (the GDI device name, e.g. `\\.\DISPLAY1`).
    pub id: String,
    /// Friendly name for the calibration screen.
    pub name: String,
    /// Bounds in the device's own physical pixels.
    pub rect: Rect,
    /// DPI scale, 1.0 = 96 DPI.
    pub scale: f32,
    pub primary: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub id: DeviceId,
    pub name: String,
    pub version: String,
    pub protocol: u16,
    pub monitors: Vec<Monitor>,
}

/// Where a device's screens sit in the shared layout.
///
/// `x`/`y` place the top-left of the device's monitor bounding box, and
/// `scale` is layout units per device pixel, so a 4K laptop at 200% and a
/// 1080p monitor at 100% end up the same physical-ish size.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub struct Placement {
    pub x: f64,
    pub y: f64,
    pub scale: f64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct LayoutDoc {
    /// Bumped on every edit. The higher revision wins when devices sync.
    pub revision: u64,
    pub placements: Vec<(DeviceId, Placement)>,
}

impl LayoutDoc {
    pub fn get(&self, id: &DeviceId) -> Option<Placement> {
        self.placements.iter().find(|(d, _)| d == id).map(|(_, p)| *p)
    }
    pub fn set(&mut self, id: DeviceId, p: Placement) {
        match self.placements.iter_mut().find(|(d, _)| *d == id) {
            Some(slot) => slot.1 = p,
            None => self.placements.push((id, p)),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Button {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    /// Path relative to the offer root, `\` separated.
    pub rel: String,
    pub size: u64,
    pub is_dir: bool,
    /// Last write time as a Windows FILETIME.
    pub modified: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileOffer {
    pub id: u64,
    /// Every file and folder in the offer, folders first.
    pub entries: Vec<FileEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClipOffer {
    pub id: u64,
    pub text: Option<String>,
    pub html: Option<Vec<u8>>,
    pub png: Option<Vec<u8>>,
    pub files: Option<FileOffer>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Msg {
    Hello(DeviceInfo),
    Monitors(Vec<Monitor>),
    Layout(LayoutDoc),

    // Controller -> controlled.
    /// The controller's cursor entered this device at (x, y).
    Enter { x: i32, y: i32, buttons: Vec<Button> },
    /// The cursor left this device. Release anything still held.
    Leave,
    Move { x: i32, y: i32 },
    Button { button: Button, down: bool, x: i32, y: i32 },
    Wheel { horizontal: bool, delta: i32 },
    Key { scan: u16, vk: u16, extended: bool, down: bool },

    // Controlled -> controller.
    /// The user touched this device's own mouse or keyboard.
    TakeBack,

    Clipboard(ClipOffer),

    /// A file drag is following the cursor onto this device. The receiver
    /// starts its own drag with virtual files from `offer`.
    DragIn { offer: FileOffer, x: i32, y: i32 },
    /// Ask the device under the cursor whether a file drag is in progress
    /// at (x, y). Answered with `DragProbeResult`.
    DragProbe { x: i32, y: i32 },
    DragProbeResult { offer: Option<FileOffer> },
    /// The drag ended on the receiving side, with or without a drop.
    DragDone { dropped: bool },

    /// Files sent from the app window or the Send To menu.
    SendFiles { offer: FileOffer },

    Ping(u64),
    Pong(u64),
    Bye,

    // Moving an AI workspace. The receiving PC asks; this PC answers.
    AiScan,
    AiInventory(crate::aiws::Inventory),
    AiRequest(crate::aiws::Selection),
    AiOffer { offer: FileOffer, manifest: crate::aiws::Manifest },
    AiError(String),
}

/// First frame on a file stream: which file of which offer, and from where.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileRequest {
    pub offer: u64,
    pub index: u32,
    pub offset: u64,
}

pub async fn write_frame<W, T>(w: &mut W, msg: &T) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = postcard::to_stdvec(msg)?;
    anyhow::ensure!(body.len() <= MAX_FRAME, "frame too large ({} bytes)", body.len());
    let mut buf = Vec::with_capacity(body.len() + 4);
    buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
    buf.extend_from_slice(&body);
    w.write_all(&buf).await?;
    Ok(())
}

pub async fn read_frame<R, T>(r: &mut R) -> anyhow::Result<T>
where
    R: AsyncRead + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await?;
    let len = u32::from_le_bytes(len) as usize;
    anyhow::ensure!(len <= MAX_FRAME, "frame too large ({len} bytes)");
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(postcard::from_bytes(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_roundtrip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let msg = Msg::Key { scan: 30, vk: 0x41, extended: false, down: true };
        write_frame(&mut a, &msg).await.unwrap();
        let back: Msg = read_frame(&mut b).await.unwrap();
        assert!(matches!(back, Msg::Key { scan: 30, down: true, .. }));
    }
}
