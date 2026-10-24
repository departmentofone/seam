//! Run the Seam engine without a window, driven by lines on stdin.
//! Handy for debugging and for testing two instances on one machine:
//!
//!   set SEAM_PROFILE=a && cargo run --example headless
//!
//! Commands: state | pair | join <addr> <code> | send <path> | sharing on|off | quit

use std::io::BufRead;
use std::sync::Arc;

use seam_core::engine::{self, Cmd, SettingsPatch, UiEvent};

fn patch() -> SettingsPatch {
    SettingsPatch {
        crossing: None,
        clipboard: None,
        sharing: None,
        start_at_login: None,
        run_elevated: None,
        onboarded: None,
        receive_dir: None,
        device_name: None,
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--ole-helper") {
        return seam_core::win::olehost::run_helper(&args[i + 1]);
    }
    tokio::runtime::Runtime::new()?.block_on(run())
}

async fn run() -> anyhow::Result<()> {
    let notify: engine::Notify = Arc::new(|ev| match ev {
        UiEvent::State(s) => {
            if std::env::var("SEAM_VERBOSE").is_ok() {
                println!("STATE peers={:?}", s.peers.iter().map(|p| (&p.name, p.connected)).collect::<Vec<_>>());
            }
        }
        other => println!("EVENT {}", serde_json::to_string(&other).unwrap_or_default()),
    });
    let eng = engine::start(notify).await?;
    println!("READY");

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    while let Some(line) = rx.recv().await {
        let parts: Vec<&str> = line.trim_start_matches('\u{feff}').split_whitespace().collect();
        match parts.as_slice() {
            ["state"] => {
                let s = eng.snapshot().unwrap();
                println!(
                    "STATE {}",
                    serde_json::json!({
                        "name": s.me.name, "port": s.me.port, "monitors": s.me.monitors.len(),
                        "peers": s.peers.iter().map(|p| serde_json::json!({"name": p.name, "connected": p.connected, "id": p.id})).collect::<Vec<_>>(),
                        "layout": s.layout, "pairing": s.pairing_code, "transfers": s.transfers, "sharing": s.sharing,
                    })
                );
            }
            ["pair"] => {
                eng.send(Cmd::BeginPairing);
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                println!("CODE {}", eng.snapshot().and_then(|s| s.pairing_code).unwrap_or_default());
            }
            ["join", addr, code] => eng.send(Cmd::PairWith { id: None, addr: Some(addr.to_string()), code: code.to_string() }),
            ["send", path] => {
                let to = eng.snapshot().and_then(|s| s.peers.iter().find(|p| p.connected).map(|p| p.id));
                match to {
                    Some(to) => eng.send(Cmd::SendFiles { to, paths: vec![path.into()] }),
                    None => println!("ERROR no connected peer"),
                }
            }
            ["ai-scan"] => {
                let from = eng.snapshot().and_then(|s| s.peers.iter().find(|p| p.connected).map(|p| p.id));
                match from {
                    Some(from) => eng.send(Cmd::AiScan { from }),
                    None => println!("ERROR no connected peer"),
                }
            }
            // Every project's sessions; files only for projects whose path contains <filter>.
            ["ai-request", filter] => {
                let s = eng.snapshot().unwrap();
                let Some(inv) = s.ai.inventory.clone() else {
                    println!("ERROR no inventory");
                    continue;
                };
                let defaults: std::collections::HashMap<_, _> = s.ai.defaults.iter().cloned().collect();
                let projects = inv
                    .projects
                    .iter()
                    .map(|p| seam_core::aiws::ProjectChoice {
                        key: p.key.clone(),
                        sessions: true,
                        files: p.files_available && p.key.to_lowercase().contains(&filter.to_lowercase()),
                        // SEAM_TEST_PROJECT_ROOT sends copied folders to a scratch place.
                        target: match std::env::var("SEAM_TEST_PROJECT_ROOT") {
                            Ok(root) if !p.scratch => {
                                let name = std::path::Path::new(&p.key).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                format!("{root}\\{name}")
                            }
                            _ => defaults.get(&p.key).cloned().unwrap_or_default(),
                        },
                    })
                    .collect();
                eng.send(Cmd::AiRequest(seam_core::aiws::Selection { tool: "claude".into(), projects }));
            }
            ["ai-local-scan"] => {
                let t = std::time::Instant::now();
                let inv = tokio::task::spawn_blocking(seam_core::aiws::claude::scan).await.unwrap();
                println!("LOCALSCAN {} projects in {:?}", inv.projects.len(), t.elapsed());
                for p in &inv.projects {
                    println!("PROJECT {} | sessions {} | files {} ({} MB, {} files)", p.key, p.sessions, p.files_available, p.file_bytes / 1_048_576, p.file_count);
                }
            }
            ["ai-projects"] => {
                let s = eng.snapshot().unwrap();
                for p in s.ai.inventory.iter().flat_map(|i| i.projects.iter()) {
                    println!("PROJECT {} | sessions {} | {} MB history | files {} ({} MB, {} files) | scratch {}", p.key, p.sessions, p.session_bytes / 1_048_576, p.files_available, p.file_bytes / 1_048_576, p.file_count, p.scratch);
                }
                println!("PROJECTS-END");
            }
            ["ai-apply"] => eng.send(Cmd::AiApply { register_all: true }),
            ["ai-undo"] => eng.send(Cmd::AiUndo),
            ["ai-state"] => {
                let s = eng.snapshot().unwrap();
                let mut ai = serde_json::to_value(&s.ai).unwrap();
                if let Some(inv) = ai.get_mut("inventory").and_then(|i| i.as_object_mut()) {
                    let n = inv.get("projects").and_then(|p| p.as_array()).map_or(0, |p| p.len());
                    inv.insert("projects".into(), serde_json::json!(format!("{n} projects")));
                }
                ai.as_object_mut().unwrap().remove("defaults");
                println!("AI {ai}");
            }
            ["sharing", v] => eng.send(Cmd::Settings(SettingsPatch { sharing: Some(*v == "on"), ..patch() })),
            ["quit"] => {
                eng.send(Cmd::Shutdown);
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                break;
            }
            _ => println!("ERROR unknown command"),
        }
    }
    Ok(())
}
