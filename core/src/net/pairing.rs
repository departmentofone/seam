//! Pairing with a 6-digit code.
//!
//! The code feeds SPAKE2, a password-authenticated key exchange: someone who
//! listens in learns nothing about the code, and someone who guesses gets one
//! try per connection. Both sides then MAC the two certificate fingerprints
//! with the shared key, which ties the code to the exact certificates in this
//! TLS session and rules out a machine in the middle.

use hmac::{Hmac, Mac};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity as SpakeId, Password, Spake2};

use crate::protocol::{read_frame, write_frame, DeviceId};

/// Wrong codes allowed per pairing window before the code is replaced.
pub const MAX_FAILURES: u32 = 5;
pub const WINDOW_SECS: u64 = 180;

#[derive(Serialize, Deserialize)]
enum PairMsg {
    Start { id: DeviceId, name: String, spake: Vec<u8> },
    Confirm(Vec<u8>),
    Rejected(String),
}

pub fn new_code() -> String {
    format!("{:06}", rand::thread_rng().gen_range(0..1_000_000u32))
}

pub struct Paired {
    pub id: DeviceId,
    pub name: String,
}

fn confirm_tag(key: &[u8], role: &[u8], client_fp: &str, server_fp: &str, client: &DeviceId, server: &DeviceId) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("hmac takes any key length");
    mac.update(b"seam-pair-confirm/1");
    mac.update(role);
    mac.update(client_fp.as_bytes());
    mac.update(server_fp.as_bytes());
    mac.update(client.as_bytes());
    mac.update(server.as_bytes());
    mac.finalize().into_bytes().to_vec()
}

fn spake(code: &str) -> (Spake2<Ed25519Group>, Vec<u8>) {
    Spake2::<Ed25519Group>::start_symmetric(&Password::new(code.as_bytes()), &SpakeId::new(b"seam-pair/1"))
}

/// Client side: we typed the code shown on the other device.
pub async fn run_client(
    conn: &quinn::Connection,
    code: &str,
    me: (DeviceId, String),
    my_fp: &str,
    their_fp: &str,
) -> anyhow::Result<Paired> {
    let (mut send, mut recv) = conn.open_bi().await?;
    let (state, outbound) = spake(code);
    write_frame(&mut send, &PairMsg::Start { id: me.0, name: me.1.clone(), spake: outbound }).await?;

    let (their_id, their_name, inbound) = match read_frame(&mut recv).await? {
        PairMsg::Start { id, name, spake } => (id, name, spake),
        PairMsg::Rejected(why) => anyhow::bail!("{why}"),
        PairMsg::Confirm(_) => anyhow::bail!("unexpected message"),
    };
    let key = state.finish(&inbound).map_err(|_| anyhow::anyhow!("pairing handshake failed"))?;

    let ours = confirm_tag(&key, b"client", my_fp, their_fp, &me.0, &their_id);
    write_frame(&mut send, &PairMsg::Confirm(ours)).await?;

    match read_frame(&mut recv).await? {
        PairMsg::Confirm(tag) => {
            let expected = confirm_tag(&key, b"server", my_fp, their_fp, &me.0, &their_id);
            anyhow::ensure!(constant_eq(&tag, &expected), "the other device could not confirm the code");
        }
        PairMsg::Rejected(why) => anyhow::bail!("{why}"),
        PairMsg::Start { .. } => anyhow::bail!("unexpected message"),
    }
    let _ = send.finish();
    Ok(Paired { id: their_id, name: their_name })
}

/// Server side: we are showing `code`. Returns Err on a wrong code.
pub async fn run_server(
    conn: &quinn::Connection,
    code: &str,
    me: (DeviceId, String),
    my_fp: &str,
    their_fp: &str,
) -> anyhow::Result<Paired> {
    let (mut send, mut recv) = conn.accept_bi().await?;
    let (their_id, their_name, inbound) = match read_frame(&mut recv).await? {
        PairMsg::Start { id, name, spake } => (id, name, spake),
        _ => anyhow::bail!("unexpected message"),
    };
    let (state, outbound) = spake(code);
    write_frame(&mut send, &PairMsg::Start { id: me.0, name: me.1.clone(), spake: outbound }).await?;
    let key = state.finish(&inbound).map_err(|_| anyhow::anyhow!("pairing handshake failed"))?;

    let tag = match read_frame(&mut recv).await? {
        PairMsg::Confirm(tag) => tag,
        _ => anyhow::bail!("unexpected message"),
    };
    let expected = confirm_tag(&key, b"client", their_fp, my_fp, &their_id, &me.0);
    // In both cases the client hangs up once it has read our answer; wait
    // for that so closing the connection does not swallow the reply.
    let linger = || tokio::time::timeout(std::time::Duration::from_secs(2), conn.closed());
    if !constant_eq(&tag, &expected) {
        let _ = write_frame(&mut send, &PairMsg::Rejected("Wrong code".into())).await;
        let _ = send.finish();
        let _ = linger().await;
        anyhow::bail!("wrong code");
    }
    let ours = confirm_tag(&key, b"server", their_fp, my_fp, &their_id, &me.0);
    write_frame(&mut send, &PairMsg::Confirm(ours)).await?;
    let _ = send.finish();
    let _ = linger().await;
    Ok(Paired { id: their_id, name: their_name })
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
