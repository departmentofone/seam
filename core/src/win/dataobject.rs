//! COM objects that let Explorer paste or drop files that still live on
//! another device. Explorer asks for a file list (FILEDESCRIPTOR) and then a
//! stream per file (FILECONTENTS); each stream pulls bytes over the network
//! only when Explorer reads it, so a 20 GB copy starts instantly and shows
//! Explorer's own progress dialog.
//!
//! Also here: the drop target that catches a local drag at a screen edge,
//! and the drop source that drives a drag that arrived from elsewhere.

use std::ffi::c_void;
use std::io::Read;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use windows::core::{implement, Ref, BOOL, HRESULT};
use windows::Win32::Foundation::{
    DATA_S_SAMEFORMATETC, DV_E_FORMATETC, DV_E_TYMED, E_FAIL, E_INVALIDARG, E_NOTIMPL, FILETIME, HGLOBAL, OLE_E_ADVISENOTSUPPORTED,
    S_FALSE, S_OK, STG_E_INVALIDFUNCTION,
};
use windows::Win32::System::Com::{
    STGM_READ, STGTY_STREAM,
    CoTaskMemAlloc, IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, ISequentialStream_Impl, IStream,
    IStream_Impl, DVASPECT_CONTENT, FORMATETC, LOCKTYPE, STATFLAG, STATFLAG_NONAME, STATSTG, STGC, STGMEDIUM, STGMEDIUM_0,
    STREAM_SEEK, STREAM_SEEK_CUR, STREAM_SEEK_END, STREAM_SEEK_SET, TYMED, TYMED_HGLOBAL, TYMED_ISTREAM,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{
    IDropSource, IDropSource_Impl, IDropTarget, IDropTarget_Impl, ReleaseStgMedium, CF_HDROP, DROPEFFECT, DROPEFFECT_COPY,
    DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::Foundation::POINTL;
use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL};
use windows::Win32::UI::Shell::{
    DragQueryFileW, IDataObjectAsyncCapability, IDataObjectAsyncCapability_Impl, SHCreateStdEnumFmtEtc, CFSTR_FILECONTENTS,
    CFSTR_FILEDESCRIPTORW, CFSTR_PREFERREDDROPEFFECT, FD_ATTRIBUTES, FD_FILESIZE, FD_PROGRESSUI, FD_UNICODE, FD_WRITESTIME,
    FILEDESCRIPTORW, HDROP,
};
use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS};
use windows::Win32::System::SystemServices::MK_LBUTTON;

use crate::protocol::{DeviceId, FileOffer};

/// Where virtual file bytes come from.
pub trait FileSource: Send + Sync {
    fn open(&self, from: DeviceId, offer: u64, index: u32, offset: u64) -> anyhow::Result<Box<dyn Read + Send>>;
}

pub struct Formats {
    pub descriptor: u16,
    pub contents: u16,
    pub effect: u16,
    /// Marks clipboard data Seam itself placed, so we never echo it back.
    pub marker: u16,
}

pub fn formats() -> &'static Formats {
    static F: std::sync::OnceLock<Formats> = std::sync::OnceLock::new();
    F.get_or_init(|| unsafe {
        Formats {
            descriptor: RegisterClipboardFormatW(CFSTR_FILEDESCRIPTORW) as u16,
            contents: RegisterClipboardFormatW(CFSTR_FILECONTENTS) as u16,
            effect: RegisterClipboardFormatW(CFSTR_PREFERREDDROPEFFECT) as u16,
            marker: RegisterClipboardFormatW(windows::core::w!("Seam Origin")) as u16,
        }
    })
}

fn fmt(cf: u16, tymed: TYMED, lindex: i32) -> FORMATETC {
    FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex, tymed: tymed.0 as u32 }
}

pub fn hglobal_from(bytes: &[u8]) -> windows::core::Result<HGLOBAL> {
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            return Err(E_FAIL.into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        let _ = GlobalUnlock(h);
        Ok(h)
    }
}

pub fn hglobal_bytes(h: HGLOBAL) -> Vec<u8> {
    unsafe {
        let size = GlobalSize(h);
        let p = GlobalLock(h) as *const u8;
        if p.is_null() {
            return Vec::new();
        }
        let v = std::slice::from_raw_parts(p, size).to_vec();
        let _ = GlobalUnlock(h);
        v
    }
}

fn hglobal_medium(h: HGLOBAL) -> STGMEDIUM {
    STGMEDIUM { tymed: TYMED_HGLOBAL.0 as u32, u: STGMEDIUM_0 { hGlobal: h }, pUnkForRelease: std::mem::ManuallyDrop::new(None) }
}

/// Paths in a CF_HDROP handle.
pub fn hdrop_paths(h: HDROP) -> Vec<PathBuf> {
    unsafe {
        let n = DragQueryFileW(h, u32::MAX, None);
        (0..n)
            .filter_map(|i| {
                let len = DragQueryFileW(h, i, None) as usize;
                let mut buf = vec![0u16; len + 1];
                DragQueryFileW(h, i, Some(&mut buf));
                let s = String::from_utf16_lossy(&buf[..len]);
                (!s.is_empty()).then(|| PathBuf::from(s))
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Virtual files

#[implement(IDataObject, IDataObjectAsyncCapability)]
pub struct VirtualFiles {
    from: DeviceId,
    offer: FileOffer,
    source: Arc<dyn FileSource>,
    /// Formats the shell stores on us during drag and paste (drag images,
    /// "performed drop effect" and so on).
    extra: Mutex<Vec<(u16, Vec<u8>)>>,
    async_mode: AtomicBool,
    in_op: AtomicBool,
}

impl VirtualFiles {
    pub fn create(from: DeviceId, offer: FileOffer, source: Arc<dyn FileSource>) -> IDataObject {
        VirtualFiles { from, offer, source, extra: Mutex::default(), async_mode: AtomicBool::new(true), in_op: AtomicBool::new(false) }
            .into()
    }

    fn descriptor(&self) -> windows::core::Result<HGLOBAL> {
        let n = self.offer.entries.len();
        let fd_size = std::mem::size_of::<FILEDESCRIPTORW>();
        let mut buf = vec![0u8; 4 + n * fd_size];
        buf[..4].copy_from_slice(&(n as u32).to_le_bytes());
        for (i, e) in self.offer.entries.iter().enumerate() {
            let mut fd = FILEDESCRIPTORW {
                dwFlags: (FD_ATTRIBUTES.0 | FD_FILESIZE.0 | FD_WRITESTIME.0 | FD_PROGRESSUI.0 | FD_UNICODE.0) as u32,
                dwFileAttributes: if e.is_dir { FILE_ATTRIBUTE_DIRECTORY.0 } else { FILE_ATTRIBUTE_NORMAL.0 },
                ftLastWriteTime: FILETIME { dwLowDateTime: e.modified as u32, dwHighDateTime: (e.modified >> 32) as u32 },
                nFileSizeHigh: (e.size >> 32) as u32,
                nFileSizeLow: e.size as u32,
                ..Default::default()
            };
            let mut name = [0u16; 260];
            for (d, s) in name.iter_mut().zip(e.rel.encode_utf16().take(259)) {
                *d = s;
            }
            fd.cFileName = name;
            let bytes = unsafe { std::slice::from_raw_parts(&fd as *const _ as *const u8, fd_size) };
            buf[4 + i * fd_size..4 + (i + 1) * fd_size].copy_from_slice(bytes);
        }
        hglobal_from(&buf)
    }

    fn offered(&self) -> Vec<FORMATETC> {
        let f = formats();
        let mut v = vec![
            fmt(f.descriptor, TYMED_HGLOBAL, -1),
            fmt(f.contents, TYMED_ISTREAM, -1),
            fmt(f.effect, TYMED_HGLOBAL, -1),
            fmt(f.marker, TYMED_HGLOBAL, -1),
        ];
        v.extend(self.extra.lock().iter().map(|(cf, _)| fmt(*cf, TYMED_HGLOBAL, -1)));
        v
    }
}

impl IDataObject_Impl for VirtualFiles_Impl {
    fn GetData(&self, pformatetcin: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        let req = unsafe { pformatetcin.as_ref() }.ok_or(windows::core::Error::from(E_INVALIDARG))?;
        let f = formats();
        let wants = |t: TYMED| req.tymed & t.0 as u32 != 0;

        if req.cfFormat == f.descriptor && wants(TYMED_HGLOBAL) {
            return Ok(hglobal_medium(self.descriptor()?));
        }
        if req.cfFormat == f.contents && wants(TYMED_ISTREAM) {
            let idx = req.lindex;
            let entry = usize::try_from(idx).ok().and_then(|i| self.offer.entries.get(i)).ok_or(windows::core::Error::from(DV_E_FORMATETC))?;
            if entry.is_dir {
                return Err(DV_E_FORMATETC.into());
            }
            let stream: IStream = NetStream {
                from: self.from,
                offer: self.offer.id,
                index: idx as u32,
                size: entry.size,
                modified: entry.modified,
                source: self.source.clone(),
                state: Mutex::new(StreamState { reader: None, pos: 0 }),
            }
            .into();
            return Ok(STGMEDIUM {
                tymed: TYMED_ISTREAM.0 as u32,
                u: STGMEDIUM_0 { pstm: std::mem::ManuallyDrop::new(Some(stream)) },
                pUnkForRelease: std::mem::ManuallyDrop::new(None),
            });
        }
        if req.cfFormat == f.effect && wants(TYMED_HGLOBAL) {
            return Ok(hglobal_medium(hglobal_from(&DROPEFFECT_COPY.0.to_le_bytes())?));
        }
        if req.cfFormat == f.marker && wants(TYMED_HGLOBAL) {
            return Ok(hglobal_medium(hglobal_from(&[1])?));
        }
        if wants(TYMED_HGLOBAL) {
            if let Some((_, data)) = self.extra.lock().iter().find(|(cf, _)| *cf == req.cfFormat) {
                return Ok(hglobal_medium(hglobal_from(data)?));
            }
        }
        Err(DV_E_FORMATETC.into())
    }

    fn GetDataHere(&self, _pformatetc: *const FORMATETC, _pmedium: *mut STGMEDIUM) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        let Some(req) = (unsafe { pformatetc.as_ref() }) else { return E_INVALIDARG };
        for o in self.offered() {
            if o.cfFormat == req.cfFormat {
                return if req.tymed & o.tymed != 0 { S_OK } else { DV_E_TYMED };
            }
        }
        DV_E_FORMATETC
    }

    fn GetCanonicalFormatEtc(&self, _pformatectin: *const FORMATETC, pformatetcout: *mut FORMATETC) -> HRESULT {
        if let Some(out) = unsafe { pformatetcout.as_mut() } {
            out.ptd = std::ptr::null_mut();
        }
        DATA_S_SAMEFORMATETC
    }

    fn SetData(&self, pformatetc: *const FORMATETC, pmedium: *const STGMEDIUM, frelease: BOOL) -> windows::core::Result<()> {
        let (Some(req), Some(medium)) = (unsafe { pformatetc.as_ref() }, unsafe { pmedium.as_ref() }) else {
            return Err(E_INVALIDARG.into());
        };
        if medium.tymed != TYMED_HGLOBAL.0 as u32 {
            return Err(DV_E_TYMED.into());
        }
        let bytes = hglobal_bytes(unsafe { medium.u.hGlobal });
        {
            let mut extra = self.extra.lock();
            extra.retain(|(cf, _)| *cf != req.cfFormat);
            extra.push((req.cfFormat, bytes));
        }
        if frelease.as_bool() {
            unsafe { ReleaseStgMedium(pmedium as *mut STGMEDIUM) };
        }
        Ok(())
    }

    fn EnumFormatEtc(&self, dwdirection: u32) -> windows::core::Result<IEnumFORMATETC> {
        if dwdirection != 1 {
            return Err(E_NOTIMPL.into());
        }
        unsafe { SHCreateStdEnumFmtEtc(&self.offered()) }
    }

    fn DAdvise(&self, _pformatetc: *const FORMATETC, _advf: u32, _padvsink: Ref<'_, IAdviseSink>) -> windows::core::Result<u32> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn DUnadvise(&self, _dwconnection: u32) -> windows::core::Result<()> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(OLE_E_ADVISENOTSUPPORTED.into())
    }
}

impl IDataObjectAsyncCapability_Impl for VirtualFiles_Impl {
    fn SetAsyncMode(&self, fdoopasync: BOOL) -> windows::core::Result<()> {
        self.async_mode.store(fdoopasync.as_bool(), Ordering::Relaxed);
        Ok(())
    }

    fn GetAsyncMode(&self) -> windows::core::Result<BOOL> {
        Ok(self.async_mode.load(Ordering::Relaxed).into())
    }

    fn StartOperation(&self, _pbcreserved: Ref<'_, windows::Win32::System::Com::IBindCtx>) -> windows::core::Result<()> {
        self.in_op.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn InOperation(&self) -> windows::core::Result<BOOL> {
        Ok(self.in_op.load(Ordering::Relaxed).into())
    }

    fn EndOperation(&self, _hresult: HRESULT, _pbcreserved: Ref<'_, windows::Win32::System::Com::IBindCtx>, _dweffects: u32) -> windows::core::Result<()> {
        self.in_op.store(false, Ordering::Relaxed);
        Ok(())
    }
}

struct StreamState {
    reader: Option<Box<dyn Read + Send>>,
    pos: u64,
}

#[implement(IStream)]
struct NetStream {
    from: DeviceId,
    offer: u64,
    index: u32,
    size: u64,
    modified: u64,
    source: Arc<dyn FileSource>,
    state: Mutex<StreamState>,
}

impl ISequentialStream_Impl for NetStream_Impl {
    fn Read(&self, pv: *mut c_void, cb: u32, pcbread: *mut u32) -> HRESULT {
        let mut st = self.state.lock();
        let want = (cb as u64).min(self.size.saturating_sub(st.pos)) as usize;
        let buf = unsafe { std::slice::from_raw_parts_mut(pv as *mut u8, cb as usize) };
        let mut got = 0usize;
        while got < want {
            if st.reader.is_none() {
                match self.source.open(self.from, self.offer, self.index, st.pos) {
                    Ok(r) => st.reader = Some(r),
                    Err(e) => {
                        tracing::warn!("file stream open failed: {e}");
                        return E_FAIL;
                    }
                }
            }
            match st.reader.as_mut().unwrap().read(&mut buf[got..want]) {
                Ok(0) => break,
                Ok(n) => {
                    got += n;
                    st.pos += n as u64;
                }
                Err(e) => {
                    tracing::warn!("file stream read failed: {e}");
                    st.reader = None;
                    return E_FAIL;
                }
            }
        }
        if let Some(out) = unsafe { pcbread.as_mut() } {
            *out = got as u32;
        }
        if got < cb as usize { S_FALSE } else { S_OK }
    }

    fn Write(&self, _pv: *const c_void, _cb: u32, _pcbwritten: *mut u32) -> HRESULT {
        STG_E_INVALIDFUNCTION
    }
}

impl IStream_Impl for NetStream_Impl {
    fn Seek(&self, dlibmove: i64, dworigin: STREAM_SEEK, plibnewposition: *mut u64) -> windows::core::Result<()> {
        let mut st = self.state.lock();
        let base = match dworigin {
            STREAM_SEEK_SET => 0i128,
            STREAM_SEEK_CUR => st.pos as i128,
            STREAM_SEEK_END => self.size as i128,
            _ => return Err(STG_E_INVALIDFUNCTION.into()),
        };
        let new = (base + dlibmove as i128).clamp(0, self.size as i128) as u64;
        if new != st.pos {
            st.reader = None;
            st.pos = new;
        }
        if let Some(out) = unsafe { plibnewposition.as_mut() } {
            *out = new;
        }
        Ok(())
    }

    fn SetSize(&self, _libnewsize: u64) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn CopyTo(&self, _pstm: Ref<'_, IStream>, _cb: u64, _pcbread: *mut u64, _pcbwritten: *mut u64) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }

    fn Commit(&self, _grfcommitflags: &STGC) -> windows::core::Result<()> {
        Ok(())
    }

    fn Revert(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn LockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: &LOCKTYPE) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn UnlockRegion(&self, _liboffset: u64, _cb: u64, _dwlocktype: u32) -> windows::core::Result<()> {
        Err(STG_E_INVALIDFUNCTION.into())
    }

    fn Stat(&self, pstatstg: *mut STATSTG, grfstatflag: &STATFLAG) -> windows::core::Result<()> {
        let out = unsafe { pstatstg.as_mut() }.ok_or(windows::core::Error::from(E_INVALIDARG))?;
        let ft = FILETIME { dwLowDateTime: self.modified as u32, dwHighDateTime: (self.modified >> 32) as u32 };
        *out = STATSTG { r#type: STGTY_STREAM.0 as u32, cbSize: self.size, mtime: ft, ctime: ft, atime: ft, grfMode: STGM_READ, ..Default::default() };
        if *grfstatflag != STATFLAG_NONAME {
            let name: Vec<u16> = format!("seam-{}", self.index).encode_utf16().chain(Some(0)).collect();
            unsafe {
                let p = CoTaskMemAlloc(name.len() * 2) as *mut u16;
                if !p.is_null() {
                    std::ptr::copy_nonoverlapping(name.as_ptr(), p, name.len());
                    out.pwcsName = windows::core::PWSTR(p);
                }
            }
        }
        Ok(())
    }

    fn Clone(&self) -> windows::core::Result<IStream> {
        Err(E_NOTIMPL.into())
    }
}

// ---------------------------------------------------------------------------
// Drag helpers

/// Catches a drag passing under it and reports the files being dragged.
/// Never accepts the drop, so nothing ever moves or gets deleted here.
#[implement(IDropTarget)]
pub struct Portal {
    pub on_files: Box<dyn Fn(Option<Vec<PathBuf>>)>,
}

impl Portal {
    fn read_paths(data: &IDataObject) -> Option<Vec<PathBuf>> {
        unsafe {
            let req = fmt(CF_HDROP.0, TYMED_HGLOBAL, -1);
            let mut medium = data.GetData(&req).ok()?;
            let paths = hdrop_paths(HDROP(medium.u.hGlobal.0));
            ReleaseStgMedium(&mut medium);
            (!paths.is_empty()).then_some(paths)
        }
    }
}

fn copy_or_none(effect: *mut DROPEFFECT) {
    if let Some(e) = unsafe { effect.as_mut() } {
        *e = if e.0 & DROPEFFECT_COPY.0 != 0 { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
    }
}

impl IDropTarget_Impl for Portal_Impl {
    fn DragEnter(&self, pdataobj: Ref<'_, IDataObject>, _grfkeystate: MODIFIERKEYS_FLAGS, _pt: &POINTL, pdweffect: *mut DROPEFFECT) -> windows::core::Result<()> {
        let paths = pdataobj.as_ref().and_then(Portal::read_paths);
        (self.on_files)(paths);
        copy_or_none(pdweffect);
        Ok(())
    }

    fn DragOver(&self, _grfkeystate: MODIFIERKEYS_FLAGS, _pt: &POINTL, pdweffect: *mut DROPEFFECT) -> windows::core::Result<()> {
        copy_or_none(pdweffect);
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn Drop(&self, _pdataobj: Ref<'_, IDataObject>, _grfkeystate: MODIFIERKEYS_FLAGS, _pt: &POINTL, pdweffect: *mut DROPEFFECT) -> windows::core::Result<()> {
        // The real drop happens on the other device.
        if let Some(e) = unsafe { pdweffect.as_mut() } {
            *e = DROPEFFECT_NONE;
        }
        Ok(())
    }
}

#[implement(IDropSource)]
pub struct DragSource {
    pub cancel: Arc<AtomicBool>,
}

impl IDropSource_Impl for DragSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        if fescapepressed.as_bool() || self.cancel.load(Ordering::Relaxed) {
            DRAGDROP_S_CANCEL
        } else if grfkeystate.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}
