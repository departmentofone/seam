//! Moving an AI workspace (settings, skills, memory, session history and the
//! project folders they belong to) from one PC to another.
//!
//! Each AI tool is an adapter. Claude (Claude Code and the Claude app) is the
//! first; others plug in by adding a module and an entry in `TOOLS`.

pub mod claude;
pub mod journal;
pub mod rewrite;

use serde::{Deserialize, Serialize};

/// Tools Seam knows about, shown in the UI. `ready` ones can be moved.
pub const TOOLS: &[(&str, &str, bool)] = &[
    ("claude", "Claude", true),
    ("codex", "Codex CLI", false),
    ("gemini", "Gemini CLI", false),
    ("cursor", "Cursor", false),
];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    pub label: String,
    pub files: u64,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    /// The working directory, or the history folder name when unknown.
    pub key: String,
    pub folder: String,
    pub cwd: Option<String>,
    pub sessions: u32,
    pub session_bytes: u64,
    /// Unix milliseconds.
    pub last_active: u64,
    /// The folder exists on the source and can be copied.
    pub files_available: bool,
    pub file_bytes: u64,
    pub file_count: u64,
    /// A Claude app scratch folder; it travels with the app data.
    pub scratch: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub tool: String,
    pub machine: String,
    /// The source user's profile folder, e.g. C:\Users\Max.
    pub home: String,
    pub appdata: String,
    pub found: bool,
    pub parts: Vec<Part>,
    pub projects: Vec<Project>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectChoice {
    pub key: String,
    pub sessions: bool,
    pub files: bool,
    /// Where the project lives on the receiving PC.
    pub target: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub tool: String,
    pub projects: Vec<ProjectChoice>,
}

/// Sent with the files so the receiver knows where everything came from.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub tool: String,
    pub machine: String,
    pub home: String,
    pub appdata: String,
    /// Claude app profile/organisation folders, most recently used first.
    pub accounts: Vec<String>,
    pub projects: Vec<ManifestProject>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestProject {
    pub key: String,
    pub folder: String,
    pub cwd: Option<String>,
    /// Index of its copied files under `files/<n>/`, if sent.
    pub files: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub added: u32,
    pub updated: u32,
    pub merged: u32,
    pub kept_local: u32,
    pub unchanged: u32,
    pub conflicts: u32,
    pub skipped: u32,
    pub project_files: u32,
    pub project_files_kept: u32,
    pub sessions_registered: u32,
    pub notes: Vec<String>,
    pub backup: Option<String>,
}

/// Folders left out of project copies because they can be rebuilt.
/// `.git` and `.claude\worktrees` are kept.
pub const SKIP_DIRS: &[&str] = &[
    ".venv", "venv", "node_modules", ".pytest_cache", "__pycache__", ".mypy_cache", ".ruff_cache", ".gradle", ".next", ".turbo", "target",
];

/// Where a project should go on this PC unless the user says otherwise.
pub fn default_target(cwd: &str, inv: &Inventory) -> String {
    let home_here = claude::target_home();
    if !inv.home.is_empty() && rewrite::is_under(cwd, &inv.home) {
        return format!("{}{}", home_here.display(), &rewrite::normalize(cwd)[rewrite::normalize(&inv.home).len()..]);
    }
    let drive = cwd.get(..3).unwrap_or("");
    if drive.len() == 3 && std::path::Path::new(drive).exists() {
        return rewrite::normalize(cwd);
    }
    let name = std::path::Path::new(cwd).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Project".into());
    format!("{}\\Projects\\{name}", home_here.display())
}

/// Is any Claude program running? Imports must wait until it is closed,
/// because Claude rewrites these files while it runs.
pub fn claude_running() -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    if std::env::var("SEAM_AI_IGNORE_RUNNING").is_ok_and(|v| v == "1") {
        return false;
    }
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return false };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut found = false;
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let name = crate::win::from_wide(&e.szExeFile);
                if name.eq_ignore_ascii_case("claude.exe") {
                    found = true;
                    break;
                }
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snap);
        found
    }
}
