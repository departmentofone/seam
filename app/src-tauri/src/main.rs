#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod admin;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use seam_core::engine::{self, Cmd, EngineHandle, SettingsPatch, Snapshot, UiEvent};
use seam_core::protocol::{DeviceId, LayoutDoc};
use serde::Serialize;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_notification::NotificationExt as _;

struct AppState {
    engine: EngineHandle,
    /// Paths handed to us by "Send to", waiting for the user to pick a device.
    pending_send: Mutex<Vec<String>>,
    told_about_tray: AtomicBool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SendRequest {
    paths: Vec<String>,
}

fn engine(app: &AppHandle) -> EngineHandle {
    app.state::<AppState>().engine.clone()
}

// Commands ----------------------------------------------------------------

#[tauri::command]
fn get_state(app: AppHandle) -> Option<Snapshot> {
    engine(&app).snapshot()
}

#[tauri::command]
fn begin_pairing(app: AppHandle) {
    engine(&app).send(Cmd::BeginPairing);
}

#[tauri::command]
fn end_pairing(app: AppHandle) {
    engine(&app).send(Cmd::EndPairing);
}

#[tauri::command]
fn pair_with(app: AppHandle, id: Option<DeviceId>, addr: Option<String>, code: String) {
    engine(&app).send(Cmd::PairWith { id, addr, code });
}

#[tauri::command]
fn unpair(app: AppHandle, id: DeviceId) {
    engine(&app).send(Cmd::Unpair(id));
}

#[tauri::command]
fn set_manual_addr(app: AppHandle, id: DeviceId, addr: Option<String>) {
    engine(&app).send(Cmd::SetManualAddr { id, addr });
}

#[tauri::command]
fn set_layout(app: AppHandle, layout: LayoutDoc) {
    engine(&app).send(Cmd::SetLayout(layout));
}

#[tauri::command]
fn update_settings(app: AppHandle, patch: SettingsPatch) -> Result<(), String> {
    if let Some(on) = patch.start_at_login {
        let current = engine(&app).snapshot().map(|s| s.run_elevated).unwrap_or(false);
        // The elevated logon task starts Seam itself; the Run key would start
        // a second, unelevated copy.
        set_autostart(&app, on && !patch.run_elevated.unwrap_or(current))?;
    }
    if let Some(elevated) = patch.run_elevated {
        let login = patch.start_at_login.or(engine(&app).snapshot().map(|s| s.start_at_login)).unwrap_or(true);
        admin::set_logon_task(elevated && login).map_err(|e| e.to_string())?;
        set_autostart(&app, login && !elevated)?;
    }
    engine(&app).send(Cmd::Settings(patch));
    Ok(())
}

fn set_autostart(app: &AppHandle, on: bool) -> Result<(), String> {
    let al = app.autolaunch();
    let res = if on { al.enable() } else { al.disable() };
    res.map_err(|e| format!("Could not change the startup setting: {e}"))
}

#[tauri::command]
fn send_files(app: AppHandle, to: DeviceId, paths: Vec<String>) {
    app.state::<AppState>().pending_send.lock().unwrap().clear();
    engine(&app).send(Cmd::SendFiles { to, paths: paths.into_iter().map(PathBuf::from).collect() });
}

#[tauri::command]
fn take_pending_send(app: AppHandle) -> Vec<String> {
    app.state::<AppState>().pending_send.lock().unwrap().clone()
}

#[tauri::command]
fn go_home(app: AppHandle) {
    engine(&app).send(Cmd::GoHome);
}

#[tauri::command]
fn clear_transfers(app: AppHandle) {
    engine(&app).send(Cmd::ClearTransfers);
}

#[tauri::command]
fn ai_scan(app: AppHandle, from: DeviceId) {
    engine(&app).send(Cmd::AiScan { from });
}

#[tauri::command]
fn ai_request(app: AppHandle, selection: seam_core::aiws::Selection) {
    engine(&app).send(Cmd::AiRequest(selection));
}

#[tauri::command]
fn ai_apply(app: AppHandle, register_all: bool) {
    engine(&app).send(Cmd::AiApply { register_all });
}

#[tauri::command]
fn ai_undo(app: AppHandle) {
    engine(&app).send(Cmd::AiUndo);
}

#[tauri::command]
fn ai_reset(app: AppHandle) {
    engine(&app).send(Cmd::AiReset);
}

#[tauri::command]
fn ai_register_local(app: AppHandle) {
    engine(&app).send(Cmd::AiRegisterLocal);
}

#[tauri::command]
fn ai_refresh(app: AppHandle) {
    engine(&app).send(Cmd::AiRefresh);
}

#[tauri::command]
fn reveal(path: String) {
    admin::reveal_in_explorer(&path);
}

#[tauri::command]
fn open_logs() {
    admin::open_folder(&seam_core::config::data_dir().join("logs"));
}

#[tauri::command]
fn restart_as_admin(app: AppHandle) -> Result<(), String> {
    admin::relaunch_elevated().map_err(|e| e.to_string())?;
    shutdown(&app);
    Ok(())
}

#[tauri::command]
fn quit(app: AppHandle) {
    shutdown(&app);
}

/// Put a big number on every monitor of this device for a moment, like the
/// Identify button in Windows display settings.
#[tauri::command]
fn identify_monitors(app: AppHandle) -> Result<(), String> {
    let Some(snap) = engine(&app).snapshot() else { return Ok(()) };
    for (i, m) in snap.me.monitors.iter().enumerate() {
        let label = format!("identify-{i}");
        if let Some(w) = app.get_webview_window(&label) {
            let _ = w.close();
        }
        let size = (220.0 * m.scale as f64) as u32;
        let url = format!("index.html#identify/{}/{}", i + 1, urlencode(&m.name));
        let win = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App(url.into()))
            .title("Seam")
            .transparent(true)
            .decorations(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .resizable(false)
            .focused(false)
            .shadow(false)
            .visible(false)
            .build()
            .map_err(|e| e.to_string())?;
        let _ = win.set_size(PhysicalSize::new(size, size));
        let _ = win.set_position(PhysicalPosition::new(m.rect.x + (m.rect.w - size as i32) / 2, m.rect.y + (m.rect.h - size as i32) / 2));
        let _ = win.show();
        let w2 = win.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(2600)).await;
            let _ = w2.close();
        });
    }
    Ok(())
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

// Shell -------------------------------------------------------------------

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn shutdown(app: &AppHandle) {
    engine(app).send(Cmd::Shutdown);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Give the engine a moment to bring the cursor home and say bye.
        tokio::time::sleep(Duration::from_millis(400)).await;
        app.exit(0);
    });
}

/// Handle `--send <paths…>` from the Send To menu.
fn handle_args(app: &AppHandle, args: &[String]) {
    let Some(pos) = args.iter().position(|a| a == "--send") else {
        if !args.iter().any(|a| a == "--minimized") {
            show_main(app);
        }
        return;
    };
    let paths: Vec<String> = args[pos + 1..].iter().filter(|a| !a.starts_with("--")).cloned().collect();
    if paths.is_empty() {
        show_main(app);
        return;
    }
    let online: Vec<DeviceId> = engine(app).snapshot().map(|s| s.peers.iter().filter(|p| p.connected).map(|p| p.id).collect()).unwrap_or_default();
    if online.len() == 1 {
        engine(app).send(Cmd::SendFiles { to: online[0], paths: paths.into_iter().map(PathBuf::from).collect() });
        return;
    }
    *app.state::<AppState>().pending_send.lock().unwrap() = paths.clone();
    show_main(app);
    let _ = app.emit("seam-send", SendRequest { paths });
}

fn on_engine_event(app: &AppHandle, ev: UiEvent) {
    match &ev {
        UiEvent::Cursor { on } => {
            if let Some(tray) = app.tray_by_id("main") {
                let tip = match on {
                    Some(name) => format!("Seam: cursor on {name}"),
                    None => "Seam: cursor on this PC".to_string(),
                };
                let _ = tray.set_tooltip(Some(tip));
            }
        }
        UiEvent::State(s) => {
            if let Some(item) = app.try_state::<TrayItems>() {
                let _ = item.pause.set_checked(!s.sharing);
            }
        }
        UiEvent::Notice { level, title, body } => {
            let visible = app.get_webview_window("main").and_then(|w| w.is_visible().ok()).unwrap_or(false);
            if !visible && (level == "ok" || level == "warn" || level == "error") {
                let _ = app.notification().builder().title(title).body(body).show();
            }
        }
        UiEvent::PairResult { .. } => {}
    }
    let _ = app.emit("seam", ev);
}

struct TrayItems {
    pause: CheckMenuItem<tauri::Wry>,
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Seam", true, None::<&str>)?;
    let pause = CheckMenuItem::with_id(app, "pause", "Pause sharing", true, false, None::<&str>)?;
    let home = MenuItem::with_id(app, "home", "Bring cursor home", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Seam", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &PredefinedMenuItem::separator(app)?, &pause, &home, &PredefinedMenuItem::separator(app)?, &quit])?;
    app.manage(TrayItems { pause: pause.clone() });

    TrayIconBuilder::with_id("main")
        .icon(app.default_window_icon().cloned().expect("app icon"))
        .tooltip("Seam")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "pause" => {
                let sharing = engine(app).snapshot().map(|s| s.sharing).unwrap_or(true);
                engine(app).send(Cmd::Settings(SettingsPatch {
                    sharing: Some(!sharing),
                    crossing: None,
                    clipboard: None,
                    start_at_login: None,
                    run_elevated: None,
                    onboarded: None,
                    receive_dir: None,
                    device_name: None,
                }));
            }
            "home" => engine(app).send(Cmd::GoHome),
            "quit" => shutdown(app),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

fn init_logging(prefix: &str) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::prelude::*;
    let dir = seam_core::config::data_dir().join("logs");
    std::fs::create_dir_all(&dir).ok()?;
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .max_log_files(7)
        .filename_prefix(prefix)
        .filename_suffix("log")
        .build(&dir)
        .ok()?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    let filter = tracing_subscriber::EnvFilter::try_from_env("SEAM_LOG").unwrap_or_else(|_| "info,mdns_sd=warn,quinn=warn".into());
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(writer).with_ansi(false))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
    Some(guard)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // The clipboard helper an elevated Seam starts at normal user level.
    if let Some(i) = args.iter().position(|a| a == "--ole-helper") {
        let _log = init_logging("seam-helper");
        if let Some(pipe) = args.get(i + 1) {
            if let Err(e) = seam_core::win::olehost::run_helper(pipe) {
                tracing::error!("clipboard helper failed: {e}");
            }
        }
        return;
    }

    let _log = init_logging("seam");
    std::panic::set_hook(Box::new(|info| tracing::error!("panic: {info}")));
    tracing::info!("Seam {} starting", env!("CARGO_PKG_VERSION"));
    // A named test profile (SEAM_PROFILE) runs beside the real app and never
    // touches startup settings.
    let dev_profile = std::env::var("SEAM_PROFILE").is_ok_and(|p| !p.is_empty());

    let mut builder = tauri::Builder::default();
    if !dev_profile {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| handle_args(app, &argv)));
    }
    builder
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--minimized"])))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(move |app| {
            // "Send to" hands paths to the running copy with WM_COPYDATA, which
            // Windows blocks from Explorer to an elevated window unless allowed.
            if seam_core::win::is_elevated() {
                use windows::Win32::UI::WindowsAndMessaging::{ChangeWindowMessageFilter, MSGFLT_ADD, WM_COPYDATA};
                unsafe {
                    let _ = ChangeWindowMessageFilter(WM_COPYDATA, MSGFLT_ADD);
                }
            }
            let handle = app.handle().clone();
            let notify_handle = handle.clone();
            let notify: engine::Notify = Arc::new(move |ev| on_engine_event(&notify_handle, ev));
            let eng = tauri::async_runtime::block_on(engine::start(notify))?;
            app.manage(AppState { engine: eng.clone(), pending_send: Mutex::new(Vec::new()), told_about_tray: AtomicBool::new(false) });
            build_tray(&handle)?;

            // First run of an installed copy: start at login unless the user
            // turned it off.
            if let Some(s) = eng.snapshot() {
                let first_run = s.start_at_login && !s.run_elevated && !s.onboarded;
                if first_run && !dev_profile && !cfg!(debug_assertions) && !handle.autolaunch().is_enabled().unwrap_or(false) {
                    let _ = handle.autolaunch().enable();
                }
            }
            handle_args(&handle, &args);
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                    let app = window.app_handle();
                    let state = app.state::<AppState>();
                    if !state.told_about_tray.swap(true, Ordering::Relaxed) {
                        let _ = app
                            .notification()
                            .builder()
                            .title("Seam is still running")
                            .body("It keeps sharing from the tray. Right-click the tray icon to quit.")
                            .show();
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            begin_pairing,
            end_pairing,
            pair_with,
            unpair,
            set_manual_addr,
            set_layout,
            update_settings,
            send_files,
            take_pending_send,
            go_home,
            clear_transfers,
            ai_scan,
            ai_request,
            ai_apply,
            ai_undo,
            ai_reset,
            ai_register_local,
            ai_refresh,
            reveal,
            open_logs,
            restart_as_admin,
            identify_monitors,
            quit,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Seam");
}
