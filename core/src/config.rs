//! Settings and paired devices, stored as JSON in %APPDATA%\Seam.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::protocol::{DeviceId, LayoutDoc, DEFAULT_PORT};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PeerRecord {
    pub id: DeviceId,
    pub name: String,
    /// SHA-256 of the peer's certificate, hex. Only this certificate is
    /// accepted for this device.
    pub fingerprint: String,
    /// Last address we reached it on, used when discovery is blocked.
    pub last_addr: Option<String>,
    /// Address typed by the user, tried first.
    pub manual_addr: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CrossingSettings {
    /// Hold the cursor against the edge this long before it crosses.
    pub delay_ms: u32,
    /// Corners this many pixels deep never cross, so reaching for a
    /// close button or the Start menu does not throw the cursor away.
    pub corner_px: u32,
    /// Let the cursor cross while a mouse button is held. File drags always
    /// cross; this covers everything else.
    pub cross_with_buttons: bool,
    /// Pointer speed multiplier on other devices.
    pub speed: f32,
    /// Pressing this key combination brings the cursor home.
    pub home_hotkey: String,
    /// Toggling this key keeps the cursor on the current device.
    pub lock_key: String,
}

impl Default for CrossingSettings {
    fn default() -> Self {
        Self {
            delay_ms: 0,
            corner_px: 12,
            cross_with_buttons: false,
            speed: 1.0,
            home_hotkey: "Ctrl+Alt+Home".into(),
            lock_key: "ScrollLock".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ClipboardSettings {
    pub text: bool,
    pub images: bool,
    pub files: bool,
    pub drag_files: bool,
    /// Skip what password managers mark as private.
    pub respect_private: bool,
    pub max_image_mb: u32,
}

impl Default for ClipboardSettings {
    fn default() -> Self {
        Self { text: true, images: true, files: true, drag_files: true, respect_private: true, max_image_mb: 32 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub device_id: DeviceId,
    pub device_name: String,
    pub port: u16,
    pub peers: Vec<PeerRecord>,
    pub layout: LayoutDoc,
    pub crossing: CrossingSettings,
    pub clipboard: ClipboardSettings,
    /// Master switch for input sharing.
    pub sharing: bool,
    pub start_at_login: bool,
    pub run_elevated: bool,
    pub onboarded: bool,
    /// Where files sent with "Send files" land.
    pub receive_dir: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            device_id: Uuid::new_v4(),
            device_name: crate::win::computer_name(),
            port: DEFAULT_PORT,
            peers: Vec::new(),
            layout: LayoutDoc::default(),
            crossing: CrossingSettings::default(),
            clipboard: ClipboardSettings::default(),
            sharing: true,
            start_at_login: true,
            run_elevated: false,
            onboarded: false,
            receive_dir: default_receive_dir().to_string_lossy().into_owned(),
        }
    }
}

fn default_receive_dir() -> PathBuf {
    let base = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Downloads").join("Seam")
}

/// The data folder. `SEAM_PROFILE` gives a second instance its own folder so
/// two copies can run on one machine during development.
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    match std::env::var("SEAM_PROFILE") {
        Ok(p) if !p.is_empty() => base.join(format!("Seam-{p}")),
        _ => base.join("Seam"),
    }
}

impl Config {
    pub fn path() -> PathBuf {
        data_dir().join("config.json")
    }

    pub fn load() -> Self {
        let path = Self::path();
        let cfg = std::fs::read(&path)
            .ok()
            .and_then(|b| match serde_json::from_slice::<Config>(&b) {
                Ok(c) => Some(c),
                Err(e) => {
                    tracing::warn!("config at {} is unreadable ({e}), starting fresh", path.display());
                    let _ = std::fs::rename(&path, path.with_extension("json.bad"));
                    None
                }
            })
            .unwrap_or_default();
        if !path.exists() {
            let _ = cfg.save();
        }
        cfg
    }

    /// Write atomically so a crash mid-save never leaves half a file.
    pub fn save(&self) -> anyhow::Result<()> {
        let path = Self::path();
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn peer(&self, id: &DeviceId) -> Option<&PeerRecord> {
        self.peers.iter().find(|p| p.id == *id)
    }

    pub fn peer_mut(&mut self, id: &DeviceId) -> Option<&mut PeerRecord> {
        self.peers.iter_mut().find(|p| p.id == *id)
    }
}
