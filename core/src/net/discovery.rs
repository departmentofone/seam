//! Finding other Seam devices on the local network with mDNS.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use parking_lot::Mutex;
use uuid::Uuid;

use crate::protocol::{DeviceId, SERVICE_TYPE};

#[derive(Clone, Debug, serde::Serialize)]
pub struct Found {
    pub id: DeviceId,
    pub name: String,
    pub addrs: Vec<SocketAddr>,
    /// The device is showing a pairing code right now.
    pub pairing: bool,
}

pub struct Discovery {
    daemon: ServiceDaemon,
    me: Mutex<(DeviceId, String, u16, bool)>,
    found: Arc<Mutex<HashMap<String, Found>>>,
}

fn instance(id: &DeviceId) -> String {
    format!("seam-{}", id.simple())
}

/// Private addresses first, so we try the home network before VPN or
/// virtual adapters.
fn rank(addr: &IpAddr) -> u8 {
    match addr {
        IpAddr::V4(v4) if v4.is_private() => 0,
        IpAddr::V4(v4) if v4.is_link_local() => 2,
        IpAddr::V4(_) => 1,
        IpAddr::V6(_) => 3,
    }
}

impl Discovery {
    pub fn start(id: DeviceId, name: String, port: u16, on_change: impl Fn() + Send + 'static) -> anyhow::Result<Arc<Self>> {
        let daemon = ServiceDaemon::new()?;
        let found: Arc<Mutex<HashMap<String, Found>>> = Arc::default();
        let me = Arc::new(Self { daemon, me: Mutex::new((id, name, port, false)), found: found.clone() });
        me.announce()?;

        let rx = me.daemon.browse(SERVICE_TYPE)?;
        std::thread::Builder::new().name("seam-mdns".into()).spawn(move || {
            while let Ok(ev) = rx.recv() {
                match ev {
                    ServiceEvent::ServiceResolved(info) => {
                        let props = info.get_properties();
                        let Some(peer) = props.get_property_val_str("id").and_then(|s| Uuid::parse_str(s).ok()) else {
                            continue;
                        };
                        if peer == id {
                            continue;
                        }
                        let mut addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
                        addrs.sort_by_key(rank);
                        let f = Found {
                            id: peer,
                            name: props.get_property_val_str("name").unwrap_or("Unknown device").to_string(),
                            addrs: addrs.into_iter().map(|a| SocketAddr::new(a, info.get_port())).collect(),
                            pairing: props.get_property_val_str("pair") == Some("1"),
                        };
                        found.lock().insert(info.get_fullname().to_string(), f);
                        on_change();
                    }
                    ServiceEvent::ServiceRemoved(_, fullname) => {
                        if found.lock().remove(&fullname).is_some() {
                            on_change();
                        }
                    }
                    _ => {}
                }
            }
        })?;
        Ok(me)
    }

    fn announce(&self) -> anyhow::Result<()> {
        let (id, name, port, pairing) = self.me.lock().clone();
        let host = format!("{}.local.", instance(&id));
        let id_s = id.to_string();
        let props = [("id", id_s.as_str()), ("name", name.as_str()), ("pair", if pairing { "1" } else { "0" }), ("v", "1")];
        let info = ServiceInfo::new(SERVICE_TYPE, &instance(&id), &host, "", port, &props[..])?.enable_addr_auto();
        self.daemon.register(info)?;
        Ok(())
    }

    pub fn set_pairing(&self, pairing: bool) {
        self.me.lock().3 = pairing;
        if let Err(e) = self.announce() {
            tracing::warn!("mdns re-announce failed: {e}");
        }
    }

    pub fn set_name(&self, name: String) {
        self.me.lock().1 = name;
        let _ = self.announce();
    }

    pub fn all(&self) -> Vec<Found> {
        let mut v: Vec<Found> = self.found.lock().values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v.dedup_by_key(|f| f.id);
        v
    }

    pub fn addrs_for(&self, id: &DeviceId) -> Vec<SocketAddr> {
        self.found.lock().values().filter(|f| f.id == *id).flat_map(|f| f.addrs.clone()).collect()
    }

    pub fn shutdown(&self) {
        let (id, ..) = self.me.lock().clone();
        let _ = self.daemon.unregister(&format!("{}.{}", instance(&id), SERVICE_TYPE));
        let _ = self.daemon.shutdown();
    }
}
