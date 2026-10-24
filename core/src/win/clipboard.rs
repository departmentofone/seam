//! The OLE thread: watches the clipboard, places clipboard contents that came
//! from other devices, and runs the drag-and-drop helpers. COM objects for
//! clipboard and drag must live on one single-threaded apartment with a
//! message loop, which is this thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use parking_lot::RwLock;
use tokio::sync::mpsc::UnboundedSender;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HANDLE, HGLOBAL, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, RemoveClipboardFormatListener, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropTarget, OleInitialize, OleIsCurrentClipboard, OleSetClipboard, OleUninitialize,
    RegisterDragDrop, RevokeDragDrop, CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_MOVE, MOUSEINPUT};
use windows::Win32::UI::Shell::HDROP;
use windows::Win32::UI::WindowsAndMessaging::*;

use super::dataobject::{formats, hglobal_bytes, hglobal_from, DragSource, FileSource, Portal, VirtualFiles};
use super::SEAM_TAG;
use crate::config::ClipboardSettings;
use crate::protocol::{ClipOffer, DeviceId, FileOffer};

const WM_APP_CMD: u32 = WM_APP + 2;
const TIMER_DEBOUNCE: usize = 1;
const TIMER_PROBE: usize = 2;

#[derive(Debug, Serialize, Deserialize)]
pub enum ClipCmd {
    /// Put what another device copied on our clipboard.
    Set { from: DeviceId, offer: ClipOffer },
    /// Is a file drag under the cursor at (x, y)?
    Probe { x: i32, y: i32 },
    PortalAt { x: i32, y: i32 },
    HidePortal,
    /// Start a drag of virtual files at (x, y). The left button must be
    /// pressed on the drag window after this (see `ClipOut::PressForDrag`).
    DragIn { from: DeviceId, offer: FileOffer, x: i32, y: i32 },
    Quit,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LocalClip {
    pub text: Option<String>,
    pub html: Option<Vec<u8>>,
    pub png: Option<Vec<u8>>,
    pub files: Option<Vec<PathBuf>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ClipOut {
    Changed(LocalClip),
    Probe(Option<Vec<PathBuf>>),
    PressForDrag { x: i32, y: i32 },
    DragInEnded { dropped: bool },
}

pub struct Clip {
    tx: Sender<ClipCmd>,
    hwnd: isize,
    /// Set to abort a drag that arrived from elsewhere (its cursor left).
    pub cancel_drag: Arc<AtomicBool>,
    pub drag_active: Arc<AtomicBool>,
}

impl Clip {
    pub fn start(
        settings: Arc<RwLock<ClipboardSettings>>,
        source: Arc<dyn FileSource>,
        out: UnboundedSender<ClipOut>,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (hwnd_tx, hwnd_rx) = std::sync::mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicBool::new(false));
        let (c, a) = (cancel.clone(), active.clone());
        std::thread::Builder::new().name("seam-ole".into()).spawn(move || {
            if let Err(e) = ole_thread(settings, source, out, rx, hwnd_tx, c, a) {
                tracing::error!("clipboard thread failed: {e}");
            }
        })?;
        let hwnd = hwnd_rx.recv_timeout(Duration::from_secs(5))?;
        Ok(Self { tx, hwnd, cancel_drag: cancel, drag_active: active })
    }

    pub fn send(&self, cmd: ClipCmd) {
        let _ = self.tx.send(cmd);
        unsafe {
            let _ = PostMessageW(Some(HWND(self.hwnd as _)), WM_APP_CMD, WPARAM(0), LPARAM(0));
        }
    }
}

impl Drop for Clip {
    fn drop(&mut self) {
        self.send(ClipCmd::Quit);
    }
}

struct Ole {
    settings: Arc<RwLock<ClipboardSettings>>,
    source: Arc<dyn FileSource>,
    out: UnboundedSender<ClipOut>,
    rx: Receiver<ClipCmd>,
    hwnd: HWND,
    portal: HWND,
    dragger: HWND,
    probing: bool,
    pending_drag: Option<(DeviceId, FileOffer)>,
    cancel: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
    cf_html: u16,
    cf_png: u16,
    cf_exclude: u16,
    cf_history: u16,
    cf_ignore: u16,
}

thread_local! {
    static OLE: std::cell::RefCell<Option<Ole>> = const { std::cell::RefCell::new(None) };
}

fn with_ole<R>(f: impl FnOnce(&mut Ole) -> R) -> Option<R> {
    OLE.with(|o| o.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f)))
}

fn ole_thread(
    settings: Arc<RwLock<ClipboardSettings>>,
    source: Arc<dyn FileSource>,
    out: UnboundedSender<ClipOut>,
    rx: Receiver<ClipCmd>,
    hwnd_tx: std::sync::mpsc::Sender<isize>,
    cancel: Arc<AtomicBool>,
    active: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    unsafe {
        OleInitialize(None)?;
        let hinst = GetModuleHandleW(None)?;

        let main_class = w!("SeamClipboard");
        RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(main_proc), hInstance: hinst.into(), lpszClassName: main_class, ..Default::default() });
        let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), main_class, PCWSTR::null(), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(hinst.into()), None)?;

        let float_class = w!("SeamPortal");
        RegisterClassW(&WNDCLASSW {
            lpfnWndProc: Some(float_proc),
            hInstance: hinst.into(),
            lpszClassName: float_class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            ..Default::default()
        });
        let make_float = || {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                float_class,
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                32,
                32,
                None,
                None,
                Some(hinst.into()),
                None,
            )
        };
        let portal = make_float()?;
        let dragger = make_float()?;
        for h in [portal, dragger] {
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), 1, LWA_ALPHA);
        }

        let probe_out = out.clone();
        let target: IDropTarget = Portal {
            on_files: Box::new(move |paths| {
                let still_probing = with_ole(|o| {
                    let was = o.probing;
                    o.probing = false;
                    let _ = KillTimer(Some(o.hwnd), TIMER_PROBE);
                    was
                })
                .unwrap_or(false);
                if still_probing {
                    let _ = probe_out.send(ClipOut::Probe(paths));
                }
            }),
        }
        .into();
        RegisterDragDrop(portal, &target)?;

        let reg = |s: PCWSTR| RegisterClipboardFormatW(s) as u16;
        OLE.with(|o| {
            *o.borrow_mut() = Some(Ole {
                settings,
                source,
                out,
                rx,
                hwnd,
                portal,
                dragger,
                probing: false,
                pending_drag: None,
                cancel,
                active,
                cf_html: reg(w!("HTML Format")),
                cf_png: reg(w!("PNG")),
                cf_exclude: reg(w!("ExcludeClipboardContentFromMonitorProcessing")),
                cf_history: reg(w!("CanIncludeInClipboardHistory")),
                cf_ignore: reg(w!("Clipboard Viewer Ignore")),
            })
        });
        let _ = formats();
        AddClipboardFormatListener(hwnd)?;
        let _ = hwnd_tx.send(hwnd.0 as isize);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let _ = RemoveClipboardFormatListener(hwnd);
        let _ = RevokeDragDrop(portal);
        // Leave the clipboard usable after we exit: anything virtual that is
        // still ours is dropped rather than flushed (its bytes are elsewhere).
        if OleIsCurrentClipboard(None).is_ok() {
            let _ = OleSetClipboard(None);
        }
        OLE.with(|o| o.borrow_mut().take());
        drop(target);
        OleUninitialize();
    }
    Ok(())
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_CLIPBOARDUPDATE => {
            let _ = SetTimer(Some(hwnd), TIMER_DEBOUNCE, 80, None);
            LRESULT(0)
        }
        WM_TIMER if w.0 == TIMER_DEBOUNCE => {
            let _ = KillTimer(Some(hwnd), TIMER_DEBOUNCE);
            with_ole(|o| o.read_clipboard());
            LRESULT(0)
        }
        WM_TIMER if w.0 == TIMER_PROBE => {
            let _ = KillTimer(Some(hwnd), TIMER_PROBE);
            with_ole(|o| {
                if o.probing {
                    o.probing = false;
                    let _ = ShowWindow(o.portal, SW_HIDE);
                    let _ = o.out.send(ClipOut::Probe(None));
                }
            });
            LRESULT(0)
        }
        WM_APP_CMD => {
            let quit = with_ole(|o| o.drain()).unwrap_or(false);
            if quit {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

unsafe extern "system" fn float_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_SETCURSOR => {
            SetCursor(None);
            LRESULT(1)
        }
        WM_LBUTTONDOWN => {
            // The left button went down on the drag window: start the drag
            // that arrived from another device.
            let pending = with_ole(|o| (o.dragger == hwnd).then(|| o.pending_drag.take()).flatten()).flatten();
            if let Some((from, offer)) = pending {
                run_drag(hwnd, from, offer);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, w, l),
    }
}

/// Blocks in DoDragDrop's own message loop until the drop or cancel.
unsafe fn run_drag(hwnd: HWND, from: DeviceId, offer: FileOffer) {
    let Some((source, cancel, active, out)) = with_ole(|o| (o.source.clone(), o.cancel.clone(), o.active.clone(), o.out.clone())) else {
        return;
    };
    cancel.store(false, Ordering::Relaxed);
    active.store(true, Ordering::Relaxed);
    let data = VirtualFiles::create(from, offer, source);
    let src: IDropSource = DragSource { cancel: cancel.clone() }.into();
    let mut effect = DROPEFFECT_NONE;
    let hr = DoDragDrop(&data, &src, DROPEFFECT_COPY, &mut effect);
    active.store(false, Ordering::Relaxed);
    let _ = ShowWindow(hwnd, SW_HIDE);
    let dropped = hr == windows::Win32::Foundation::DRAGDROP_S_DROP && effect != DROPEFFECT(0);
    let _ = out.send(ClipOut::DragInEnded { dropped });
}

unsafe fn show_at(hwnd: HWND, x: i32, y: i32) {
    let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x - 16, y - 16, 32, 32, SWP_NOACTIVATE | SWP_SHOWWINDOW);
}

/// Nudge the cursor by one pixel and back so a drag loop notices the window
/// that just appeared under it.
fn wiggle() {
    let mk = |dx: i32| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx, dy: 0, mouseData: 0, dwFlags: MOUSEEVENTF_MOVE, time: 0, dwExtraInfo: SEAM_TAG } },
    };
    unsafe {
        SendInput(&[mk(1), mk(-1)], std::mem::size_of::<INPUT>() as i32);
    }
}

impl Ole {
    fn drain(&mut self) -> bool {
        while let Ok(cmd) = self.rx.try_recv() {
            unsafe {
                match cmd {
                    ClipCmd::Set { from, offer } => self.set_clipboard(from, offer),
                    ClipCmd::Probe { x, y } => {
                        self.probing = true;
                        show_at(self.portal, x, y);
                        let _ = SetTimer(Some(self.hwnd), TIMER_PROBE, 400, None);
                        wiggle();
                    }
                    ClipCmd::PortalAt { x, y } => show_at(self.portal, x, y),
                    ClipCmd::HidePortal => {
                        self.probing = false;
                        let _ = ShowWindow(self.portal, SW_HIDE);
                    }
                    ClipCmd::DragIn { from, offer, x, y } => {
                        self.pending_drag = Some((from, offer));
                        show_at(self.dragger, x, y);
                        let _ = self.out.send(ClipOut::PressForDrag { x, y });
                    }
                    ClipCmd::Quit => return true,
                }
            }
        }
        false
    }

    fn open(&self) -> bool {
        for _ in 0..10 {
            if unsafe { OpenClipboard(Some(self.hwnd)) }.is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        false
    }

    fn read_clipboard(&mut self) {
        let s = self.settings.read().clone();
        unsafe {
            if IsClipboardFormatAvailable(formats().marker as u32).is_ok() {
                return;
            }
            if s.respect_private
                && (IsClipboardFormatAvailable(self.cf_exclude as u32).is_ok() || IsClipboardFormatAvailable(self.cf_ignore as u32).is_ok())
            {
                return;
            }
            if !self.open() {
                tracing::debug!("clipboard busy, skipping update");
                return;
            }
            let get = |cf: u16| GetClipboardData(cf as u32).ok().filter(|h| !h.is_invalid()).map(|h| HGLOBAL(h.0));

            if s.respect_private {
                if let Some(h) = get(self.cf_history) {
                    let v = hglobal_bytes(h);
                    if v.len() >= 4 && u32::from_le_bytes(v[..4].try_into().unwrap()) == 0 {
                        let _ = CloseClipboard();
                        return;
                    }
                }
            }

            let mut clip = LocalClip { text: None, html: None, png: None, files: None };
            if s.files {
                if let Some(h) = get(CF_HDROP.0) {
                    let paths = super::dataobject::hdrop_paths(HDROP(h.0));
                    if !paths.is_empty() {
                        clip.files = Some(paths);
                    }
                }
            }
            if clip.files.is_none() {
                if s.text {
                    if let Some(h) = get(CF_UNICODETEXT.0) {
                        let raw = hglobal_bytes(h);
                        let units: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&c| c != 0).collect();
                        clip.text = Some(String::from_utf16_lossy(&units));
                    }
                    if let Some(h) = get(self.cf_html) {
                        let mut v = hglobal_bytes(h);
                        while v.last() == Some(&0) {
                            v.pop();
                        }
                        clip.html = Some(v);
                    }
                }
                if s.images {
                    let limit = s.max_image_mb as usize * 1024 * 1024;
                    if let Some(h) = get(self.cf_png) {
                        let v = hglobal_bytes(h);
                        if v.len() <= limit {
                            clip.png = Some(v);
                        }
                    } else if let Some(h) = get(CF_DIBV5.0).or_else(|| get(CF_DIB.0)) {
                        let dib = hglobal_bytes(h);
                        if dib.len() <= limit * 2 {
                            clip.png = crate::image::dib_to_png(&dib).filter(|p| p.len() <= limit);
                        }
                    }
                }
            }
            let _ = CloseClipboard();
            if clip.text.is_some() || clip.png.is_some() || clip.files.is_some() {
                let _ = self.out.send(ClipOut::Changed(clip));
            }
        }
    }

    unsafe fn set_clipboard(&mut self, from: DeviceId, offer: ClipOffer) {
        if let Some(files) = offer.files {
            let data = VirtualFiles::create(from, files, self.source.clone());
            if let Err(e) = OleSetClipboard(&data) {
                tracing::warn!("could not place files on the clipboard: {e}");
            }
            return;
        }
        if !self.open() {
            tracing::warn!("clipboard busy, could not paste from another device");
            return;
        }
        let _ = EmptyClipboard();
        let put = |cf: u16, bytes: &[u8]| {
            if let Ok(h) = hglobal_from(bytes) {
                if SetClipboardData(cf as u32, Some(HANDLE(h.0))).is_err() {
                    let _ = windows::Win32::Foundation::GlobalFree(Some(h));
                }
            }
        };
        if let Some(text) = &offer.text {
            let bytes: Vec<u8> = text.encode_utf16().chain(Some(0)).flat_map(|u| u.to_le_bytes()).collect();
            put(CF_UNICODETEXT.0, &bytes);
        }
        if let Some(html) = &offer.html {
            let mut v = html.clone();
            v.push(0);
            put(self.cf_html, &v);
        }
        if let Some(png) = &offer.png {
            put(self.cf_png, png);
            if let Some(dib) = crate::image::png_to_dib(png) {
                put(CF_DIB.0, &dib);
            }
        }
        put(formats().marker, &[1]);
        let _ = CloseClipboard();
    }
}
