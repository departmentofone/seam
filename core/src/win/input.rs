//! Mouse and keyboard sharing.
//!
//! The controlling device runs low-level hooks on a dedicated thread. While
//! the cursor is on this device, events pass through untouched; when it runs
//! off an edge that leads to another device, the hook swallows local input,
//! parks the real cursor out of sight and turns each movement into an
//! absolute position on the other device. The controlled device only injects
//! what it receives, on its own injector thread.
//!
//! Everything in the hook callback must stay fast: Windows silently removes
//! a low-level hook that keeps it waiting, so nothing here blocks.

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use parking_lot::{Mutex, RwLock};
use tokio::sync::mpsc::UnboundedSender;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::SEAM_TAG;
use crate::config::CrossingSettings;
use crate::layout::{Desk, Edge, Hit, Step};
use crate::protocol::{Button, DeviceId, FileOffer, Msg};

const WM_APP_CMD: u32 = WM_APP + 1;
/// Real movement on a controlled device beyond this many pixels hands
/// control back to its own mouse.
const TAKEBACK_PX: i32 = 6;
const PROBE_TIMEOUT: Duration = Duration::from_millis(1200);

#[derive(Debug)]
pub enum InputOut {
    Send { to: DeviceId, msg: Msg },
    /// Where the cursor is now. `None` means on this device.
    Active(Option<DeviceId>),
    /// Another device is driving this one (or stopped).
    ControlledBy(Option<DeviceId>),
    Locked(bool),
    MonitorsChanged,
    /// The left button is held against an edge that leads somewhere: check
    /// whether a file drag is under the cursor here.
    ProbeLocalDrag { x: i32, y: i32 },
    /// Continue a drag that started on `from`, with its files, at (x, y).
    StartDragIn { from: DeviceId, offer: FileOffer, x: i32, y: i32 },
    /// Keep the drop portal under the parked cursor while a drag is away.
    PortalAt { x: i32, y: i32 },
    HidePortal,
    /// An elevated window has focus and we are not elevated.
    ElevatedForeground,
}

#[derive(Debug)]
pub enum HookCmd {
    TakeBack { from: DeviceId },
    PeerGone(DeviceId),
    GoHome,
    LocalDrag(Option<FileOffer>),
    RemoteDrag { from: DeviceId, offer: Option<FileOffer> },
    Quit,
}

#[derive(Debug)]
pub enum InjectCmd {
    Enter { by: DeviceId, x: i32, y: i32, buttons: Vec<Button> },
    Move { x: i32, y: i32 },
    Button { button: Button, down: bool, x: i32, y: i32 },
    Wheel { horizontal: bool, delta: i32 },
    Key { scan: u16, vk: u16, extended: bool, down: bool },
    Leave,
    /// Press or release a button here for our own purposes (starting a drag).
    Local { button: Button, down: bool, x: i32, y: i32 },
    Quit,
}

pub struct Shared {
    pub me: DeviceId,
    pub desk: RwLock<Arc<Desk>>,
    pub crossing: RwLock<CrossingSettings>,
    pub drag_files: AtomicBool,
    pub enabled: AtomicBool,
    pub elevated: bool,
    controlled_by: Mutex<Option<DeviceId>>,
    last_injected: Mutex<(i32, i32)>,
}

impl Shared {
    pub fn new(me: DeviceId, crossing: CrossingSettings, drag_files: bool, enabled: bool) -> Arc<Self> {
        Arc::new(Self {
            me,
            desk: RwLock::new(Arc::new(Desk::default())),
            crossing: RwLock::new(crossing),
            drag_files: AtomicBool::new(drag_files),
            enabled: AtomicBool::new(enabled),
            elevated: super::is_elevated(),
            controlled_by: Mutex::new(None),
            last_injected: Mutex::new((0, 0)),
        })
    }

    pub fn controlled_by(&self) -> Option<DeviceId> {
        *self.controlled_by.lock()
    }
}

pub struct Input {
    pub shared: Arc<Shared>,
    hook_tid: u32,
    hook_tx: Sender<HookCmd>,
    inject_tx: Sender<InjectCmd>,
}

impl Input {
    pub fn start(shared: Arc<Shared>, out: UnboundedSender<InputOut>) -> anyhow::Result<Self> {
        let (hook_tx, hook_rx) = crossbeam_channel::unbounded();
        let (inject_tx, inject_rx) = crossbeam_channel::unbounded();
        let (tid_tx, tid_rx) = std::sync::mpsc::channel();

        let s = shared.clone();
        let o = out.clone();
        std::thread::Builder::new().name("seam-hooks".into()).spawn(move || {
            let _ = tid_tx.send(unsafe { GetCurrentThreadId() });
            if let Err(e) = hook_thread(s, o, hook_rx) {
                tracing::error!("input hooks failed: {e}");
            }
        })?;
        let hook_tid = tid_rx.recv_timeout(Duration::from_secs(5))?;

        let s = shared.clone();
        std::thread::Builder::new().name("seam-inject".into()).spawn(move || injector(s, out, inject_rx))?;

        Ok(Self { shared, hook_tid, hook_tx, inject_tx })
    }

    pub fn hook(&self, cmd: HookCmd) {
        let _ = self.hook_tx.send(cmd);
        unsafe {
            let _ = PostThreadMessageW(self.hook_tid, WM_APP_CMD, WPARAM(0), LPARAM(0));
        }
    }

    pub fn inject(&self, cmd: InjectCmd) {
        let _ = self.inject_tx.send(cmd);
    }

    /// A handle for injecting from another task.
    pub fn injector(&self) -> Sender<InjectCmd> {
        self.inject_tx.clone()
    }

    pub fn set_desk(&self, desk: Desk) {
        *self.shared.desk.write() = Arc::new(desk);
        // Wake the hook thread so it can notice a device that went away.
        unsafe {
            let _ = PostThreadMessageW(self.hook_tid, WM_APP_CMD, WPARAM(0), LPARAM(0));
        }
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        self.hook(HookCmd::Quit);
        self.inject(InjectCmd::Quit);
    }
}

// ---------------------------------------------------------------------------
// Hook thread

#[derive(Debug)]
enum Mode {
    Local,
    Remote { dev: DeviceId, x: f64, y: f64 },
}

#[derive(Debug)]
enum Drag {
    Idle,
    /// Waiting to learn whether the local left-button hold is a file drag.
    Probing(Instant),
    NoDrag,
    Ready(FileOffer),
    /// A local file drag followed the cursor to another device.
    Carrying,
    /// Same three states for a drag that started on the remote device.
    RemoteProbing(Instant),
    RemoteReady(FileOffer),
    RemoteNoDrag,
}

#[derive(Default)]
struct Mods {
    ctrl: bool,
    alt: bool,
    shift: bool,
    win: bool,
}

impl Mods {
    fn update(&mut self, vk: u32, down: bool) {
        match VIRTUAL_KEY(vk as u16) {
            VK_LCONTROL | VK_RCONTROL | VK_CONTROL => self.ctrl = down,
            VK_LMENU | VK_RMENU | VK_MENU => self.alt = down,
            VK_LSHIFT | VK_RSHIFT | VK_SHIFT => self.shift = down,
            VK_LWIN | VK_RWIN => self.win = down,
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    pub vk: u32,
}

impl Hotkey {
    pub fn parse(s: &str) -> Option<Self> {
        let mut hk = Hotkey { ctrl: false, alt: false, shift: false, win: false, vk: 0 };
        for part in s.split('+').map(|p| p.trim().to_ascii_lowercase()) {
            match part.as_str() {
                "ctrl" | "control" => hk.ctrl = true,
                "alt" => hk.alt = true,
                "shift" => hk.shift = true,
                "win" => hk.win = true,
                key => hk.vk = key_vk(key)?,
            }
        }
        (hk.vk != 0).then_some(hk)
    }

    fn matches(&self, vk: u32, m: &Mods) -> bool {
        self.vk == vk && self.ctrl == m.ctrl && self.alt == m.alt && self.shift == m.shift && self.win == m.win
    }
}

pub fn key_vk(name: &str) -> Option<u32> {
    let vk = match name {
        "home" => VK_HOME,
        "end" => VK_END,
        "insert" => VK_INSERT,
        "delete" => VK_DELETE,
        "pageup" => VK_PRIOR,
        "pagedown" => VK_NEXT,
        "pause" => VK_PAUSE,
        "scrolllock" => VK_SCROLL,
        "capslock" => VK_CAPITAL,
        "escape" | "esc" => VK_ESCAPE,
        "space" => VK_SPACE,
        "none" | "" => return None,
        k if k.len() == 1 => {
            let c = k.chars().next()?.to_ascii_uppercase();
            if c.is_ascii_alphanumeric() {
                return Some(c as u32);
            }
            return None;
        }
        k if k.starts_with('f') => {
            let n: u32 = k[1..].parse().ok()?;
            if (1..=24).contains(&n) {
                return Some(VK_F1.0 as u32 + n - 1);
            }
            return None;
        }
        _ => return None,
    };
    Some(vk.0 as u32)
}

struct Hooks {
    shared: Arc<Shared>,
    out: UnboundedSender<InputOut>,
    rx: Receiver<HookCmd>,
    mode: Mode,
    /// Where the real cursor actually sits while input is going elsewhere.
    anchor: (i32, i32),
    /// Where the cursor left this device, for coming home by hotkey.
    home: (i32, i32),
    hider: HWND,
    drag: Drag,
    edge_since: Option<(Edge, Instant)>,
    locked: bool,
    mods: Mods,
    /// Buttons and keys currently held and delivered here.
    local_buttons: HashSet<Button>,
    local_keys: HashSet<u32>,
    /// Held here when the cursor left. Their release must reach this device.
    pass_buttons: HashSet<Button>,
    pass_keys: HashSet<u32>,
    /// Held on the remote device through us.
    remote_buttons: HashSet<Button>,
    /// Hotkey presses whose key-up we must also swallow.
    eat_up: HashSet<u32>,
}

thread_local! {
    static HOOKS: RefCell<Option<Hooks>> = const { RefCell::new(None) };
}

fn hook_thread(shared: Arc<Shared>, out: UnboundedSender<InputOut>, rx: Receiver<HookCmd>) -> anyhow::Result<()> {
    unsafe {
        let hinst = GetModuleHandleW(None)?;
        let class = w!("SeamCursorHider");
        let wc = WNDCLASSW { lpfnWndProc: Some(hider_proc), hInstance: hinst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        let hider = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            class,
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            24,
            24,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        // Nearly transparent but still hit-testable, so the cursor over it
        // asks this window for its shape, and we answer "none".
        let _ = SetLayeredWindowAttributes(hider, COLORREF(0), 1, LWA_ALPHA);
        HIDER_OUT.with(|o| *o.borrow_mut() = Some(out.clone()));

        HOOKS.with(|h| {
            *h.borrow_mut() = Some(Hooks {
                shared,
                out,
                rx,
                mode: Mode::Local,
                anchor: (0, 0),
                home: (0, 0),
                hider,
                drag: Drag::Idle,
                edge_since: None,
                locked: false,
                mods: Mods::default(),
                local_buttons: HashSet::new(),
                local_keys: HashSet::new(),
                pass_buttons: HashSet::new(),
                pass_keys: HashSet::new(),
                remote_buttons: HashSet::new(),
                eat_up: HashSet::new(),
            })
        });

        let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hinst.into()), 0)?;
        let keyboard = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(hinst.into()), 0)?;
        tracing::info!("input hooks installed");

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.hwnd.is_invalid() && msg.message == WM_APP_CMD {
                let quit = HOOKS.with(|h| h.borrow_mut().as_mut().map_or(false, |s| s.drain_commands()));
                if quit {
                    break;
                }
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // Never leave the cursor stranded on another device.
        HOOKS.with(|h| {
            if let Some(s) = h.borrow_mut().as_mut() {
                s.go_home(None, false);
            }
        });
        let _ = UnhookWindowsHookEx(mouse);
        let _ = UnhookWindowsHookEx(keyboard);
        let _ = DestroyWindow(hider);
    }
    Ok(())
}

thread_local! {
    static HIDER_OUT: RefCell<Option<UnboundedSender<InputOut>>> = const { RefCell::new(None) };
}

unsafe extern "system" fn hider_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_SETCURSOR => {
            SetCursor(None);
            LRESULT(1)
        }
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => {
            HIDER_OUT.with(|o| {
                if let Some(o) = o.borrow().as_ref() {
                    let _ = o.send(InputOut::MonitorsChanged);
                }
            });
            DefWindowProcW(hwnd, msg, w, l)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

unsafe extern "system" fn mouse_proc(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = &*(l.0 as *const MSLLHOOKSTRUCT);
        let eat = HOOKS.with(|h| match h.try_borrow_mut() {
            Ok(mut g) => g.as_mut().map_or(false, |s| s.on_mouse(w.0 as u32, info)),
            Err(_) => false,
        });
        if eat {
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, w, l)
}

unsafe extern "system" fn keyboard_proc(code: i32, w: WPARAM, l: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = &*(l.0 as *const KBDLLHOOKSTRUCT);
        let eat = HOOKS.with(|h| match h.try_borrow_mut() {
            Ok(mut g) => g.as_mut().map_or(false, |s| s.on_key(w.0 as u32, info)),
            Err(_) => false,
        });
        if eat {
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, w, l)
}

fn decode_button(msg: u32, data: u32) -> Option<(Button, bool)> {
    let x = || if (data >> 16) as u16 == XBUTTON1 { Button::X1 } else { Button::X2 };
    Some(match msg {
        WM_LBUTTONDOWN => (Button::Left, true),
        WM_LBUTTONUP => (Button::Left, false),
        WM_RBUTTONDOWN => (Button::Right, true),
        WM_RBUTTONUP => (Button::Right, false),
        WM_MBUTTONDOWN => (Button::Middle, true),
        WM_MBUTTONUP => (Button::Middle, false),
        WM_XBUTTONDOWN => (x(), true),
        WM_XBUTTONUP => (x(), false),
        _ => return None,
    })
}

impl Hooks {
    fn send(&self, to: DeviceId, msg: Msg) {
        let _ = self.out.send(InputOut::Send { to, msg });
    }

    fn emit(&self, ev: InputOut) {
        let _ = self.out.send(ev);
    }

    fn on_mouse(&mut self, msg: u32, info: &MSLLHOOKSTRUCT) -> bool {
        if info.dwExtraInfo == SEAM_TAG {
            return false;
        }
        let pt = (info.pt.x, info.pt.y);
        let physical = info.flags & LLMHF_INJECTED == 0;

        // Someone else is driving this device: a real nudge of our own
        // mouse takes it back.
        if let Some(by) = self.shared.controlled_by() {
            if physical {
                let take = match msg {
                    WM_MOUSEMOVE => {
                        let li = *self.shared.last_injected.lock();
                        (pt.0 - li.0).abs() + (pt.1 - li.1).abs() > TAKEBACK_PX
                    }
                    WM_MOUSEWHEEL | WM_MOUSEHWHEEL => true,
                    m => decode_button(m, info.mouseData).is_some_and(|(_, down)| down),
                };
                if take {
                    *self.shared.controlled_by.lock() = None;
                    self.send(by, Msg::TakeBack);
                    self.emit(InputOut::ControlledBy(None));
                }
            }
            return false;
        }

        match self.mode {
            Mode::Local => self.local_mouse(msg, info, pt),
            Mode::Remote { .. } => self.remote_mouse(msg, info, pt),
        }
    }

    fn local_mouse(&mut self, msg: u32, info: &MSLLHOOKSTRUCT, pt: (i32, i32)) -> bool {
        if let Some((b, down)) = decode_button(msg, info.mouseData) {
            if down {
                self.local_buttons.insert(b);
            } else {
                self.local_buttons.remove(&b);
                if b == Button::Left && !matches!(self.drag, Drag::Idle) {
                    self.drag = Drag::Idle;
                    self.emit(InputOut::HidePortal);
                }
            }
            return false;
        }
        if msg != WM_MOUSEMOVE || self.locked || !self.shared.enabled.load(Ordering::Relaxed) {
            return false;
        }

        let desk = self.shared.desk.read().clone();
        let me = self.shared.me;
        let Some(edge) = self.edge_at(&desk, pt) else {
            self.edge_since = None;
            return false;
        };
        let settings = self.shared.crossing.read().clone();

        if !self.local_buttons.is_empty() {
            let maybe_file_drag = self.local_buttons.len() == 1
                && self.local_buttons.contains(&Button::Left)
                && self.shared.drag_files.load(Ordering::Relaxed);
            if maybe_file_drag {
                match &self.drag {
                    Drag::Idle => {
                        self.drag = Drag::Probing(Instant::now());
                        self.emit(InputOut::ProbeLocalDrag { x: pt.0, y: pt.1 });
                        return false;
                    }
                    Drag::Probing(since) => {
                        if since.elapsed() > PROBE_TIMEOUT {
                            self.drag = Drag::NoDrag;
                        }
                        return false;
                    }
                    Drag::Ready(_) => {}
                    _ if !settings.cross_with_buttons => return false,
                    _ => {}
                }
            } else if !settings.cross_with_buttons {
                return false;
            }
        }

        if settings.delay_ms > 0 {
            match self.edge_since {
                Some((e, t)) if e == edge => {
                    if t.elapsed() < Duration::from_millis(settings.delay_ms as u64) {
                        return false;
                    }
                }
                _ => {
                    self.edge_since = Some((edge, Instant::now()));
                    return false;
                }
            }
        }

        let Some(hit) = desk.cross(&me, pt.0, pt.1, edge) else { return false };
        self.enter_remote(hit, pt);
        true
    }

    /// The edge under `pt` that leads to another device, outside the
    /// corner dead zones.
    fn edge_at(&self, desk: &Desk, pt: (i32, i32)) -> Option<Edge> {
        let me = self.shared.me;
        let dev = desk.device(&me)?;
        let mon = dev.monitor_at(pt.0, pt.1)?;
        let r = mon.rect;
        let corner = self.shared.crossing.read().corner_px as i32;
        let (x, y) = pt;
        let near_v = y - r.y < corner || r.bottom() - 1 - y < corner;
        let near_h = x - r.x < corner || r.right() - 1 - x < corner;
        let candidates = [
            (x <= r.x && !near_v, Edge::Left),
            (x >= r.right() - 1 && !near_v, Edge::Right),
            (y <= r.y && !near_h, Edge::Top),
            (y >= r.bottom() - 1 && !near_h, Edge::Bottom),
        ];
        candidates.into_iter().filter(|(on, _)| *on).map(|(_, e)| e).find(|e| desk.edge_has_target(&me, x, y, *e))
    }

    fn enter_remote(&mut self, hit: Hit, pt: (i32, i32)) {
        self.home = pt;
        self.edge_since = None;
        self.pass_buttons = self.local_buttons.clone();
        self.pass_keys = self.local_keys.clone();

        let offer = match std::mem::replace(&mut self.drag, Drag::Idle) {
            Drag::Ready(o) => Some(o),
            _ => None,
        };
        let carry_buttons: Vec<Button> =
            if offer.is_some() { Vec::new() } else { self.local_buttons.iter().copied().collect() };
        self.remote_buttons = carry_buttons.iter().copied().collect();
        self.send(hit.device, Msg::Enter { x: hit.x, y: hit.y, buttons: carry_buttons });
        if let Some(offer) = offer {
            self.send(hit.device, Msg::DragIn { offer, x: hit.x, y: hit.y });
            self.remote_buttons.insert(Button::Left);
            self.drag = Drag::Carrying;
        }
        self.mode = Mode::Remote { dev: hit.device, x: hit.x as f64, y: hit.y as f64 };
        self.park();
        self.emit(InputOut::Active(Some(hit.device)));
    }

    /// Move the real cursor to the middle of its screen, where no edge can
    /// clip its movement, and hide it there.
    fn park(&mut self) {
        let desk = self.shared.desk.read().clone();
        let center = desk
            .device(&self.shared.me)
            .and_then(|d| d.nearest_monitor(self.anchor_hint().0, self.anchor_hint().1).map(|m| m.rect))
            .map(|r| (r.x + r.w / 2, r.y + r.h / 2))
            .unwrap_or((200, 200));
        unsafe {
            if matches!(self.drag, Drag::Carrying) {
                self.emit(InputOut::PortalAt { x: center.0, y: center.1 });
            } else {
                let _ = SetWindowPos(
                    self.hider,
                    Some(HWND_TOPMOST),
                    center.0 - 12,
                    center.1 - 12,
                    24,
                    24,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
            let _ = SetCursorPos(center.0, center.1);
        }
        self.anchor = center;
    }

    fn anchor_hint(&self) -> (i32, i32) {
        self.home
    }

    fn unpark(&mut self, to: (i32, i32)) {
        unsafe {
            let _ = ShowWindow(self.hider, SW_HIDE);
            let _ = SetCursorPos(to.0, to.1);
        }
        self.emit(InputOut::HidePortal);
    }

    fn remote_mouse(&mut self, msg: u32, info: &MSLLHOOKSTRUCT, pt: (i32, i32)) -> bool {
        let Mode::Remote { dev, x, y } = self.mode else { return false };

        if msg == WM_MOUSEMOVE {
            let (dx, dy) = (pt.0 - self.anchor.0, pt.1 - self.anchor.1);
            if dx == 0 && dy == 0 {
                return true;
            }
            let desk = self.shared.desk.read().clone();
            let speed = self.shared.crossing.read().speed.clamp(0.25, 4.0) as f64;
            let ratio = match (desk.device(&self.shared.me), desk.device(&dev)) {
                (Some(a), Some(b)) => a.place.scale / b.place.scale * speed,
                _ => speed,
            };
            match desk.step(&dev, x, y, dx as f64 * ratio, dy as f64 * ratio) {
                Step::Stay { x: nx, y: ny } => {
                    self.mode = Mode::Remote { dev, x: nx, y: ny };
                    if (nx as i32, ny as i32) != (x as i32, y as i32) {
                        self.send(dev, Msg::Move { x: nx as i32, y: ny as i32 });
                    }
                }
                Step::Cross(hit) if !self.locked => self.cross_from_remote(dev, (x, y), hit),
                Step::Cross(_) => {}
            }
            return true;
        }

        if let Some((b, down)) = decode_button(msg, info.mouseData) {
            if !down && self.pass_buttons.remove(&b) {
                // Pressed here before the cursor left: let the release land
                // here too, and on the other side if it was carried over.
                if self.remote_buttons.remove(&b) {
                    self.send(dev, Msg::Button { button: b, down: false, x: x as i32, y: y as i32 });
                }
                if b == Button::Left && matches!(self.drag, Drag::Carrying) {
                    self.drag = Drag::Idle;
                }
                return false;
            }
            if down {
                self.remote_buttons.insert(b);
            } else {
                self.remote_buttons.remove(&b);
                if b == Button::Left && !matches!(self.drag, Drag::Carrying) {
                    self.drag = Drag::Idle;
                }
            }
            self.send(dev, Msg::Button { button: b, down, x: x as i32, y: y as i32 });
            return true;
        }

        match msg {
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
                let delta = (info.mouseData >> 16) as u16 as i16 as i32;
                self.send(dev, Msg::Wheel { horizontal: msg == WM_MOUSEHWHEEL, delta });
                true
            }
            _ => true,
        }
    }

    fn cross_from_remote(&mut self, dev: DeviceId, at: (f64, f64), hit: Hit) {
        let me = self.shared.me;
        let settings = self.shared.crossing.read().clone();
        let held = !self.remote_buttons.is_empty();

        if !held || matches!(self.drag, Drag::Carrying) {
            // Carrying means our own drag is coming back with the cursor.
            if hit.device == me {
                self.go_home(Some(hit), false);
            } else {
                self.switch_remote(dev, hit);
            }
            return;
        }

        let left_only = self.remote_buttons.len() == 1 && self.remote_buttons.contains(&Button::Left);
        if hit.device == me && left_only && self.shared.drag_files.load(Ordering::Relaxed) {
            match std::mem::replace(&mut self.drag, Drag::Idle) {
                Drag::Idle => {
                    self.send(dev, Msg::DragProbe { x: at.0 as i32, y: at.1 as i32 });
                    self.drag = Drag::RemoteProbing(Instant::now());
                }
                Drag::RemoteProbing(since) => {
                    self.drag = if since.elapsed() > PROBE_TIMEOUT { Drag::RemoteNoDrag } else { Drag::RemoteProbing(since) };
                }
                Drag::RemoteReady(offer) => {
                    self.go_home(Some(hit), true);
                    let p = self.inset(hit);
                    self.emit(InputOut::StartDragIn { from: dev, offer, x: p.0, y: p.1 });
                }
                Drag::RemoteNoDrag => {
                    self.drag = Drag::RemoteNoDrag;
                    if settings.cross_with_buttons {
                        self.go_home(Some(hit), false);
                    }
                }
                other => self.drag = other,
            }
            return;
        }

        if settings.cross_with_buttons {
            if hit.device == me {
                self.go_home(Some(hit), false);
            } else {
                self.switch_remote(dev, hit);
            }
        }
    }

    fn switch_remote(&mut self, from: DeviceId, hit: Hit) {
        self.send(from, Msg::Leave);
        let buttons: Vec<Button> = self.remote_buttons.iter().copied().collect();
        self.send(hit.device, Msg::Enter { x: hit.x, y: hit.y, buttons });
        self.mode = Mode::Remote { dev: hit.device, x: hit.x as f64, y: hit.y as f64 };
        self.emit(InputOut::Active(Some(hit.device)));
    }

    /// Step `hit` a couple of pixels away from any edge that leads elsewhere,
    /// so the cursor does not bounce straight back out.
    fn inset(&self, hit: Hit) -> (i32, i32) {
        let desk = self.shared.desk.read().clone();
        let me = self.shared.me;
        let (mut x, mut y) = (hit.x, hit.y);
        if let Some(r) = desk.device(&me).and_then(|d| d.nearest_monitor(x, y)).map(|m| m.rect) {
            if x <= r.x {
                x = r.x + 2;
            }
            if x >= r.right() - 1 {
                x = r.right() - 3;
            }
            if y <= r.y {
                y = r.y + 2;
            }
            if y >= r.bottom() - 1 {
                y = r.bottom() - 3;
            }
        }
        (x, y)
    }

    /// Bring the cursor back. `hit` is where it crosses in; `None` means
    /// back to where it left.
    fn go_home(&mut self, hit: Option<Hit>, keep_drag: bool) {
        let Mode::Remote { dev, .. } = self.mode else { return };
        self.send(dev, Msg::Leave);
        let to = match hit {
            Some(h) => self.inset(h),
            None => self.inset(Hit { device: self.shared.me, x: self.home.0, y: self.home.1 }),
        };
        self.mode = Mode::Local;
        self.remote_buttons.clear();
        if !keep_drag {
            self.drag = Drag::Idle;
        }
        // Keys held here before leaving are still physically down; keep
        // tracking them as local.
        self.local_keys = std::mem::take(&mut self.pass_keys);
        self.local_buttons = std::mem::take(&mut self.pass_buttons);
        self.unpark(to);
        self.emit(InputOut::Active(None));
    }

    fn on_key(&mut self, msg: u32, info: &KBDLLHOOKSTRUCT) -> bool {
        if info.dwExtraInfo == SEAM_TAG {
            return false;
        }
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let vk = info.vkCode;
        self.mods.update(vk, down);

        if !down && self.eat_up.remove(&vk) {
            return true;
        }
        let settings = self.shared.crossing.read().clone();
        if down {
            if let Some(hk) = Hotkey::parse(&settings.home_hotkey) {
                if hk.matches(vk, &self.mods) && matches!(self.mode, Mode::Remote { .. }) {
                    self.release_remote_modifiers();
                    self.go_home(None, false);
                    self.eat_up.insert(vk);
                    return true;
                }
            }
            if key_vk(&settings.lock_key.to_ascii_lowercase()) == Some(vk) && !self.local_keys.contains(&vk) {
                self.locked = !self.locked;
                self.emit(InputOut::Locked(self.locked));
            }
        }

        match self.mode {
            Mode::Local => {
                if down {
                    self.local_keys.insert(vk);
                } else {
                    self.local_keys.remove(&vk);
                }
                false
            }
            Mode::Remote { dev, .. } => {
                if !down && self.pass_keys.remove(&vk) {
                    return false;
                }
                let extended = info.flags.0 & LLKHF_EXTENDED.0 != 0;
                self.send(dev, Msg::Key { scan: info.scanCode as u16, vk: vk as u16, extended, down });
                true
            }
        }
    }

    /// The home hotkey's modifiers went down on the remote device; lift them
    /// there before leaving.
    fn release_remote_modifiers(&self) {
        let Mode::Remote { dev, .. } = self.mode else { return };
        let mods = [(self.mods.ctrl, VK_LCONTROL), (self.mods.alt, VK_LMENU), (self.mods.shift, VK_LSHIFT), (self.mods.win, VK_LWIN)];
        for (held, vk) in mods {
            if held {
                let scan = unsafe { MapVirtualKeyW(vk.0 as u32, MAPVK_VK_TO_VSC) } as u16;
                self.send(dev, Msg::Key { scan, vk: vk.0, extended: vk == VK_LWIN, down: false });
            }
        }
    }

    /// Returns true when the thread should exit.
    fn drain_commands(&mut self) -> bool {
        while let Ok(cmd) = self.rx.try_recv() {
            match cmd {
                HookCmd::TakeBack { from } | HookCmd::PeerGone(from) => {
                    if matches!(self.mode, Mode::Remote { dev, .. } if dev == from) {
                        self.go_home(None, false);
                    }
                }
                HookCmd::GoHome => self.go_home(None, false),
                HookCmd::LocalDrag(offer) => {
                    if matches!(self.drag, Drag::Probing(_)) {
                        self.drag = match offer {
                            Some(o) => Drag::Ready(o),
                            None => Drag::NoDrag,
                        };
                    }
                }
                HookCmd::RemoteDrag { from, offer } => {
                    let ours = matches!(self.mode, Mode::Remote { dev, .. } if dev == from);
                    if ours && matches!(self.drag, Drag::RemoteProbing(_)) {
                        self.drag = match offer {
                            Some(o) => Drag::RemoteReady(o),
                            None => Drag::RemoteNoDrag,
                        };
                    }
                }
                HookCmd::Quit => {
                    unsafe { PostQuitMessage(0) };
                    return true;
                }
            }
        }
        // A device we were driving dropped out of the layout.
        if let Mode::Remote { dev, .. } = self.mode {
            if self.shared.desk.read().device(&dev).is_none() {
                self.go_home(None, false);
            }
        }
        if !self.shared.enabled.load(Ordering::Relaxed) {
            self.go_home(None, false);
        }
        false
    }
}

// ---------------------------------------------------------------------------
// Injector thread

fn injector(shared: Arc<Shared>, out: UnboundedSender<InputOut>, rx: Receiver<InjectCmd>) {
    let mut held_buttons: HashSet<Button> = HashSet::new();
    let mut held_keys: HashSet<(u16, u16, bool)> = HashSet::new();
    let mut last_elev_check = Instant::now() - Duration::from_secs(10);
    let mut warned_hwnd = HWND::default();

    let check_elevation = |last: &mut Instant, warned: &mut HWND| {
        if shared.elevated || last.elapsed() < Duration::from_secs(1) {
            return;
        }
        *last = Instant::now();
        let fg = unsafe { GetForegroundWindow() };
        if fg != *warned && super::window_is_elevated(fg) {
            *warned = fg;
            let _ = out.send(InputOut::ElevatedForeground);
        }
    };

    while let Ok(cmd) = rx.recv() {
        let controlled = shared.controlled_by().is_some();
        match cmd {
            InjectCmd::Enter { by, x, y, buttons } => {
                *shared.controlled_by.lock() = Some(by);
                let _ = out.send(InputOut::ControlledBy(Some(by)));
                move_abs(&shared, x, y);
                for b in buttons {
                    button(&shared, b, true, x, y);
                    held_buttons.insert(b);
                }
                check_elevation(&mut last_elev_check, &mut warned_hwnd);
            }
            InjectCmd::Move { x, y } if controlled => move_abs(&shared, x, y),
            InjectCmd::Button { button: b, down, x, y } if controlled => {
                button(&shared, b, down, x, y);
                if down {
                    held_buttons.insert(b);
                } else {
                    held_buttons.remove(&b);
                }
                check_elevation(&mut last_elev_check, &mut warned_hwnd);
            }
            InjectCmd::Wheel { horizontal, delta } if controlled => wheel(horizontal, delta),
            InjectCmd::Key { scan, vk, extended, down } if controlled => {
                key(scan, vk, extended, down);
                if down {
                    held_keys.insert((scan, vk, extended));
                } else {
                    held_keys.remove(&(scan, vk, extended));
                }
                check_elevation(&mut last_elev_check, &mut warned_hwnd);
            }
            InjectCmd::Leave => {
                let pos = *shared.last_injected.lock();
                for (scan, vk, ext) in held_keys.drain() {
                    key(scan, vk, ext, false);
                }
                for b in held_buttons.drain() {
                    button(&shared, b, false, pos.0, pos.1);
                }
                if shared.controlled_by.lock().take().is_some() {
                    let _ = out.send(InputOut::ControlledBy(None));
                }
            }
            InjectCmd::Local { button: b, down, x, y } => {
                button(&shared, b, down, x, y);
                if down {
                    held_buttons.insert(b);
                } else {
                    held_buttons.remove(&b);
                }
            }
            InjectCmd::Quit => break,
            _ => {}
        }
    }
    let pos = *shared.last_injected.lock();
    for (scan, vk, ext) in held_keys.drain() {
        key(scan, vk, ext, false);
    }
    for b in held_buttons.drain() {
        button(&shared, b, false, pos.0, pos.1);
    }
}

fn send_inputs(inputs: &[INPUT]) {
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn mouse_input(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy, mouseData: data, dwFlags: flags, time: 0, dwExtraInfo: SEAM_TAG } },
    }
}

/// Absolute coordinates for SendInput: 0..65535 across the virtual desktop.
fn normalize(shared: &Shared, x: i32, y: i32) -> (i32, i32) {
    let desk = shared.desk.read().clone();
    let (l, t, r, b) = desk.device(&shared.me).map(|d| d.bounds()).unwrap_or_else(|| unsafe {
        let l = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let t = GetSystemMetrics(SM_YVIRTUALSCREEN);
        (l, t, l + GetSystemMetrics(SM_CXVIRTUALSCREEN), t + GetSystemMetrics(SM_CYVIRTUALSCREEN))
    });
    let w = (r - l - 1).max(1) as i64;
    let h = (b - t - 1).max(1) as i64;
    let nx = (((x - l) as i64 * 65535) + w / 2) / w;
    let ny = (((y - t) as i64 * 65535) + h / 2) / h;
    (nx.clamp(0, 65535) as i32, ny.clamp(0, 65535) as i32)
}

const ABS: MOUSE_EVENT_FLAGS = MOUSE_EVENT_FLAGS(MOUSEEVENTF_MOVE.0 | MOUSEEVENTF_ABSOLUTE.0 | MOUSEEVENTF_VIRTUALDESK.0);

fn move_abs(shared: &Shared, x: i32, y: i32) {
    let (nx, ny) = normalize(shared, x, y);
    send_inputs(&[mouse_input(nx, ny, 0, ABS | MOUSEEVENTF_MOVE_NOCOALESCE)]);
    *shared.last_injected.lock() = (x, y);
}

fn button(shared: &Shared, b: Button, down: bool, x: i32, y: i32) {
    let (nx, ny) = normalize(shared, x, y);
    let (flag, data) = match (b, down) {
        (Button::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (Button::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
        (Button::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (Button::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (Button::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (Button::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (Button::X1, true) => (MOUSEEVENTF_XDOWN, XBUTTON1 as u32),
        (Button::X1, false) => (MOUSEEVENTF_XUP, XBUTTON1 as u32),
        (Button::X2, true) => (MOUSEEVENTF_XDOWN, XBUTTON2 as u32),
        (Button::X2, false) => (MOUSEEVENTF_XUP, XBUTTON2 as u32),
    };
    send_inputs(&[mouse_input(nx, ny, data, ABS | flag)]);
    *shared.last_injected.lock() = (x, y);
}

fn wheel(horizontal: bool, delta: i32) {
    let flag = if horizontal { MOUSEEVENTF_HWHEEL } else { MOUSEEVENTF_WHEEL };
    send_inputs(&[mouse_input(0, 0, delta as u32, flag)]);
}

fn key(scan: u16, vk: u16, extended: bool, down: bool) {
    let mut flags = KEYBD_EVENT_FLAGS(0);
    let (wvk, wscan) = if scan != 0 {
        flags |= KEYEVENTF_SCANCODE;
        (VIRTUAL_KEY(0), scan)
    } else {
        (VIRTUAL_KEY(vk), 0)
    };
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: wvk, wScan: wscan, dwFlags: flags, time: 0, dwExtraInfo: SEAM_TAG } },
    };
    send_inputs(&[input]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hotkeys() {
        let hk = Hotkey::parse("Ctrl+Alt+Home").unwrap();
        assert!(hk.ctrl && hk.alt && !hk.shift);
        assert_eq!(hk.vk, VK_HOME.0 as u32);
        assert_eq!(Hotkey::parse("Ctrl+F5").unwrap().vk, VK_F5.0 as u32);
        assert!(Hotkey::parse("Ctrl+Alt").is_none());
        assert_eq!(key_vk("scrolllock"), Some(VK_SCROLL.0 as u32));
    }
}
