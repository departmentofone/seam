//! Running as administrator, and small shell helpers.
//!
//! Windows blocks input from normal apps into apps running as administrator.
//! The fix is to run Seam elevated, which at login means a scheduled task
//! with "highest privileges" (the Run key cannot elevate).

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;

use windows::core::{HSTRING, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

const TASK: &str = "Seam";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn exe() -> anyhow::Result<String> {
    Ok(std::env::current_exe()?.to_string_lossy().into_owned())
}

/// Run `file args` elevated (one UAC prompt) and wait for it.
fn run_elevated(file: &str, args: &str) -> anyhow::Result<u32> {
    let verb = HSTRING::from("runas");
    let file = HSTRING::from(file);
    let params = HSTRING::from(args);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe {
        ShellExecuteExW(&mut info).map_err(|_| anyhow::anyhow!("Administrator permission was not given."))?;
        if info.hProcess.is_invalid() {
            return Ok(0);
        }
        let mut code = 0u32;
        if WaitForSingleObject(info.hProcess, 60_000) == WAIT_OBJECT_0 {
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
        }
        let _ = CloseHandle(info.hProcess);
        Ok(code)
    }
}

fn task_exists() -> bool {
    Command::new("schtasks")
        .args(["/Query", "/TN", TASK])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .is_ok_and(|o| o.status.success())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Create or remove the elevated logon task.
pub fn set_logon_task(enabled: bool) -> anyhow::Result<()> {
    if !enabled {
        if task_exists() {
            run_elevated("schtasks.exe", &format!("/Delete /TN {TASK} /F"))?;
        }
        return Ok(());
    }
    let user = std::env::var("USERDOMAIN").map(|d| format!("{d}\\")).unwrap_or_default() + &std::env::var("USERNAME").unwrap_or_default();
    // Built from XML because the schtasks flags cannot turn off "only start
    // on AC power", which would stop Seam starting on a laptop on battery.
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Starts Seam with administrator rights so it can control admin windows.</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>5</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>--minimized</Arguments></Exec></Actions>
</Task>"#,
        user = xml_escape(&user),
        exe = xml_escape(&exe()?),
    );
    let path = std::env::temp_dir().join("seam-task.xml");
    let utf16: Vec<u8> = [0xFFu8, 0xFE].into_iter().chain(xml.encode_utf16().flat_map(|u| u.to_le_bytes())).collect();
    std::fs::write(&path, utf16)?;
    let code = run_elevated("schtasks.exe", &format!("/Create /TN {TASK} /XML \"{}\" /F", path.display()));
    let _ = std::fs::remove_file(&path);
    match code? {
        0 => Ok(()),
        c => anyhow::bail!("Windows could not create the startup task (code {c})."),
    }
}

/// Start an elevated copy of Seam once this one has exited.
pub fn relaunch_elevated() -> anyhow::Result<()> {
    let exe = exe()?.replace('\'', "''");
    let script = format!("Start-Sleep -Milliseconds 900; Start-Process -FilePath '{exe}' -ArgumentList '--after-restart'");
    let verb = HSTRING::from("runas");
    let file = HSTRING::from("powershell.exe");
    let params = HSTRING::from(format!("-NoProfile -WindowStyle Hidden -Command \"{script}\""));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        lpVerb: PCWSTR(verb.as_ptr()),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(|_| anyhow::anyhow!("Administrator permission was not given."))
}

pub fn reveal_in_explorer(path: &str) {
    let _ = Command::new("explorer.exe").raw_arg(format!("/select,\"{path}\"")).spawn();
}

pub fn open_folder(path: &Path) {
    let _ = std::fs::create_dir_all(path);
    let _ = Command::new("explorer.exe").arg(path).spawn();
}
