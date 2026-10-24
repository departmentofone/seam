//! Windows platform glue.

pub mod clipboard;
pub mod dataobject;
pub mod input;
pub mod olehost;

use std::collections::HashMap;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LPARAM, RECT, ERROR_SUCCESS, LocalFree};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN};
use windows::Win32::Security::{GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY};
use windows::Win32::System::SystemInformation::{ComputerNameDnsHostname, GetComputerNameExW};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI};
use windows::core::BOOL;

use crate::protocol::{Monitor, Rect};

/// Marks input we inject so our own hooks can tell it from real input.
pub const SEAM_TAG: usize = 0x5EA4_0001;

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub fn computer_name() -> String {
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    unsafe {
        if GetComputerNameExW(ComputerNameDnsHostname, Some(PWSTR(buf.as_mut_ptr())), &mut len).is_ok() {
            return from_wide(&buf[..len as usize]);
        }
    }
    std::env::var("COMPUTERNAME").unwrap_or_else(|_| "This PC".into())
}

/// Coordinates everywhere in Seam are physical pixels.
pub fn set_dpi_awareness() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn dpapi_protect(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptProtectData(&input, PCWSTR::null(), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)?;
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as _)));
        Ok(v)
    }
}

pub fn dpapi_unprotect(data: &[u8]) -> anyhow::Result<Vec<u8>> {
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out)?;
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(out.pbData as _)));
        Ok(v)
    }
}

pub fn is_elevated() -> bool {
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elev = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elev.TokenIsElevated != 0
    }
}

/// Start `exe args` at normal user level from an elevated process, by
/// borrowing the token of the desktop shell (Explorer). Returns the process
/// handle; the caller closes it.
pub fn spawn_unelevated(exe: &str, args: &str) -> anyhow::Result<HANDLE> {
    use windows::Win32::Security::{DuplicateTokenEx, SecurityImpersonation, TokenPrimary, TOKEN_ACCESS_MASK, TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_SESSIONID, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE};
    use windows::Win32::System::Threading::{CreateProcessWithTokenW, OpenProcess, CREATE_UNICODE_ENVIRONMENT, CREATE_PROCESS_LOGON_FLAGS, PROCESS_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION, STARTUPINFOW};
    use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};
    unsafe {
        let shell = GetShellWindow();
        anyhow::ensure!(!shell.is_invalid(), "no desktop shell running");
        let mut pid = 0u32;
        GetWindowThreadProcessId(shell, Some(&mut pid));
        anyhow::ensure!(pid != 0, "no desktop shell process");
        let proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)?;
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(proc, TOKEN_DUPLICATE, &mut token);
        let _ = CloseHandle(proc);
        opened?;
        let mut primary = HANDLE::default();
        let access = TOKEN_ACCESS_MASK(TOKEN_QUERY.0 | TOKEN_ASSIGN_PRIMARY.0 | TOKEN_DUPLICATE.0 | TOKEN_ADJUST_DEFAULT.0 | TOKEN_ADJUST_SESSIONID.0);
        let dup = DuplicateTokenEx(token, access, None, SecurityImpersonation, TokenPrimary, &mut primary);
        let _ = CloseHandle(token);
        dup?;
        let mut cmd = wide(&format!("\"{exe}\" {args}"));
        let app = wide(exe);
        let si = STARTUPINFOW { cb: std::mem::size_of::<STARTUPINFOW>() as u32, ..Default::default() };
        let mut pi = PROCESS_INFORMATION::default();
        let res = CreateProcessWithTokenW(
            primary,
            CREATE_PROCESS_LOGON_FLAGS(0),
            PCWSTR(app.as_ptr()),
            Some(PWSTR(cmd.as_mut_ptr())),
            CREATE_UNICODE_ENVIRONMENT,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        );
        let _ = CloseHandle(primary);
        res?;
        let _ = CloseHandle(pi.hThread);
        Ok(pi.hProcess)
    }
}

/// Whether `hwnd` belongs to a process running as administrator. Input we
/// inject cannot reach such windows unless we are elevated too. A process we
/// may not even query counts as elevated.
pub fn window_is_elevated(hwnd: windows::Win32::Foundation::HWND) -> bool {
    use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
    if hwnd.is_invalid() {
        return false;
    }
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return false;
        }
        let Ok(proc) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else { return true };
        let mut token = HANDLE::default();
        let opened = OpenProcessToken(proc, TOKEN_QUERY, &mut token).is_ok();
        let _ = CloseHandle(proc);
        if !opened {
            return true;
        }
        let mut elev = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elev.TokenIsElevated != 0
    }
}

/// Friendly monitor names ("DELL U2720Q") keyed by GDI name (`\\.\DISPLAY1`).
fn friendly_names() -> HashMap<String, String> {
    let mut out = HashMap::new();
    unsafe {
        let mut npaths = 0u32;
        let mut nmodes = 0u32;
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut npaths, &mut nmodes) != ERROR_SUCCESS {
            return out;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); npaths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); nmodes as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut npaths, paths.as_mut_ptr(), &mut nmodes, modes.as_mut_ptr(), None)
            != ERROR_SUCCESS
        {
            return out;
        }
        for p in paths.iter().take(npaths as usize) {
            let mut src = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                continue;
            }
            let mut tgt = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            tgt.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            tgt.header.adapterId = p.targetInfo.adapterId;
            tgt.header.id = p.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut tgt.header) != 0 {
                continue;
            }
            let name = from_wide(&tgt.monitorFriendlyDeviceName);
            if !name.is_empty() {
                out.insert(from_wide(&src.viewGdiDeviceName), name);
            }
        }
    }
    out
}

pub fn monitors() -> Vec<Monitor> {
    unsafe extern "system" fn cb(hmon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = &mut *(data.0 as *mut Vec<(HMONITOR, MONITORINFOEXW)>);
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(hmon, &mut info as *mut _ as *mut MONITORINFO).as_bool() {
            list.push((hmon, info));
        }
        BOOL(1)
    }

    let mut raw: Vec<(HMONITOR, MONITORINFOEXW)> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut raw as *mut _ as isize));
    }
    let names = friendly_names();
    let mut out: Vec<Monitor> = raw
        .into_iter()
        .enumerate()
        .map(|(i, (hmon, info))| {
            let r = info.monitorInfo.rcMonitor;
            let (mut dx, mut dy) = (96u32, 96u32);
            unsafe {
                let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
            }
            let id = from_wide(&info.szDevice);
            let name = names.get(&id).cloned().unwrap_or_else(|| format!("Display {}", i + 1));
            Monitor {
                id,
                name,
                rect: Rect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top },
                scale: dx as f32 / 96.0,
                primary: info.monitorInfo.dwFlags & 1 != 0,
            }
        })
        .collect();
    out.sort_by_key(|m| (m.rect.x, m.rect.y));
    out
}

/// True if any connected network is set to Public. Windows Firewall blocks
/// incoming connections on public networks by default, and it is a hint that
/// the user is on a network they do not control.
pub fn on_public_network() -> bool {
    use windows::Win32::Networking::NetworkListManager::{INetworkListManager, NetworkListManager, NLM_ENUM_NETWORK_CONNECTED, NLM_NETWORK_CATEGORY_PUBLIC};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let Ok(nlm) = CoCreateInstance::<_, INetworkListManager>(&NetworkListManager, None, CLSCTX_ALL) else { return false };
        let Ok(nets) = nlm.GetNetworks(NLM_ENUM_NETWORK_CONNECTED) else { return false };
        loop {
            let mut item = [None];
            let mut fetched = 0u32;
            if nets.Next(&mut item, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            if let Some(net) = item[0].take() {
                if net.GetCategory().is_ok_and(|c| c == NLM_NETWORK_CATEGORY_PUBLIC) {
                    return true;
                }
            }
        }
        false
    }
}
