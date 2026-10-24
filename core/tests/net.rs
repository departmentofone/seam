//! Two Seam network stacks talking over loopback.

use std::net::SocketAddr;
use std::time::Duration;

use seam_core::identity::Identity;
use seam_core::net::{Net, NetEvent};
use seam_core::protocol::{DeviceInfo, FileRequest, Msg, PROTOCOL_VERSION};
use tokio::sync::mpsc::UnboundedReceiver;
use uuid::Uuid;

fn info(id: Uuid, name: &str) -> DeviceInfo {
    DeviceInfo { id, name: name.into(), version: "test".into(), protocol: PROTOCOL_VERSION, monitors: vec![] }
}

async fn wait_for<F: Fn(&NetEvent) -> bool>(rx: &mut UnboundedReceiver<NetEvent>, what: &str, f: F) -> NetEvent {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let ev = tokio::time::timeout_at(deadline, rx.recv()).await.unwrap_or_else(|_| panic!("timed out waiting for {what}")).unwrap();
        if std::env::var("SEAM_TEST_LOG").is_ok() {
            eprintln!("[{what}] {ev:?}");
        }
        if f(&ev) {
            return ev;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pair_connect_and_transfer() {
    let (a_id, b_id) = (Uuid::new_v4(), Uuid::new_v4());
    let (a_tx, mut a_rx) = tokio::sync::mpsc::unbounded_channel();
    let (b_tx, mut b_rx) = tokio::sync::mpsc::unbounded_channel();
    let a = Net::start(info(a_id, "A"), Identity::ephemeral(&a_id).unwrap(), 0, vec![], a_tx).await.unwrap();
    let b = Net::start(info(b_id, "B"), Identity::ephemeral(&b_id).unwrap(), 0, vec![], b_tx).await.unwrap();
    let a_addr: SocketAddr = format!("127.0.0.1:{}", a.port()).parse().unwrap();

    // Not pairing yet: refused.
    assert!(b.pair_with(vec![a_addr], "123456").await.is_err());

    let code = a.begin_pairing();
    let wrong = if code == "000000" { "111111" } else { "000000" };
    let err = b.pair_with(vec![a_addr], wrong).await.unwrap_err();
    assert!(err.to_string().to_lowercase().contains("code"), "unexpected error: {err}");
    wait_for(&mut a_rx, "pairing failure", |e| matches!(e, NetEvent::PairingFailed { closed: false })).await;

    let rec = b.pair_with(vec![a_addr], &code).await.unwrap();
    assert_eq!(rec.id, a_id);
    assert_eq!(rec.name, "A");
    let NetEvent::Paired(back) = wait_for(&mut a_rx, "paired", |e| matches!(e, NetEvent::Paired(_))).await else { unreachable!() };
    assert_eq!(back.id, b_id);
    assert_eq!(back.fingerprint, b.fingerprint());

    // Both dial loops now find each other.
    wait_for(&mut a_rx, "A connected", |e| matches!(e, NetEvent::Connected { info, .. } if info.id == b_id)).await;
    wait_for(&mut b_rx, "B connected", |e| matches!(e, NetEvent::Connected { info, .. } if info.id == a_id)).await;

    // Control messages flow.
    assert!(a.send(&b_id, Msg::Move { x: 10, y: 20 }));
    wait_for(&mut b_rx, "move", |e| matches!(e, NetEvent::Msg { msg: Msg::Move { x: 10, y: 20 }, .. })).await;

    // File streams: B asks A for a file; A's engine would serve it.
    let payload: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    let req_task = {
        let b = b.clone();
        tokio::spawn(async move {
            let mut recv = b.open_file(&a_id, FileRequest { offer: 7, index: 0, offset: 0 }).await.unwrap();
            recv.read_to_end(10_000_000).await.unwrap()
        })
    };
    let NetEvent::FileRequest { from, req, mut send } = wait_for(&mut a_rx, "file request", |e| matches!(e, NetEvent::FileRequest { .. })).await
    else {
        unreachable!()
    };
    assert_eq!(from, b_id);
    assert_eq!(req.offer, 7);
    send.write_all(&payload).await.unwrap();
    send.finish().unwrap();
    let got = req_task.await.unwrap();
    assert_eq!(got.len(), payload.len());
    assert!(got == payload);

    // Unpairing on A drops the link and B cannot get back in.
    a.set_paired(vec![]);
    wait_for(&mut b_rx, "B disconnected", |e| matches!(e, NetEvent::Disconnected { .. })).await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!b.is_connected(&a_id));

    a.shutdown();
    b.shutdown();
}
