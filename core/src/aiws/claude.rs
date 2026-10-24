//! Claude: Claude Code (~\.claude, ~\.claude.json) and the Claude app's
//! session list (%APPDATA%\Claude). Ported from ClaudeWorkspaceMerge.ps1,
//! whose merge rules were worked out on a real move between two PCs.
//!
//! What moves: settings, instructions, skills, plugins, hooks, memory,
//! session history, the app's session entries and scratch folders, MCP
//! servers and per-folder permissions from .claude.json. What never moves:
//! sign-in data, caches and live state.
//!
//! How it merges: new files are added; transcripts keep whichever copy
//! contains the other; settings and JSON configs are merged key by key with
//! this PC winning ties; a memory that differs is kept as a second file and
//! linked from MEMORY.md; anything else that differs keeps this PC's copy
//! and saves the incoming one under the backup's conflicts folder. Every
//! change is journaled so the import can be undone.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;
use serde_json::{json, Map, Value};

use super::journal::Journal;
use super::rewrite::{is_under, map_folder_name, normalize, project_folder_name, Rewriter};
use super::{Inventory, Manifest, ManifestProject, Part, Project, Report, Selection, SKIP_DIRS};

const SKIP_HOME_FOLDERS: &[&str] = &[
    "cache", "session-env", "sessions", "backups", "spares", "shell-snapshots", "statsig", "telemetry", "debug", "ide", "daemon",
];
const SKIP_HOME_FILES: &[&str] = &[".credentials.json", ".last-cleanup", ".last-update-result.json", "daemon.log", "settings.json.bak"];
const APPDATA_FOLDERS: &[&str] = &["claude-code-sessions", "scratch-workspaces"];
const TEXT_EXTS: &[&str] = &["jsonl", "json", "md", "txt"];
const MAX_WALK: usize = 400_000;

// Locations ---------------------------------------------------------------

fn env_path(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var).or_else(|| std::env::var_os(fallback)).map(PathBuf::from).unwrap_or_default()
}

pub fn source_home() -> PathBuf {
    env_path("SEAM_AI_SOURCE_HOME", "USERPROFILE")
}
fn source_appdata() -> PathBuf {
    env_path("SEAM_AI_SOURCE_APPDATA", "APPDATA")
}
/// Overridable so tests can import into a scratch folder.
pub fn target_home() -> PathBuf {
    env_path("SEAM_AI_TARGET_HOME", "USERPROFILE")
}
fn target_appdata() -> PathBuf {
    env_path("SEAM_AI_TARGET_APPDATA", "APPDATA")
}

/// The Claude app's data folder. The Microsoft Store build keeps it under
/// Packages\...\LocalCache while recording the usual %APPDATA% paths.
fn app_folder(appdata: &Path) -> PathBuf {
    let roaming = appdata.join("Claude");
    if roaming.join("claude-code-sessions").exists() {
        return roaming;
    }
    if let Some(local) = appdata.parent().map(|p| p.join("Local").join("Packages")) {
        if let Ok(rd) = std::fs::read_dir(&local) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().contains("Claude") {
                    let c = e.path().join("LocalCache").join("Roaming").join("Claude");
                    if c.join("claude-code-sessions").exists() {
                        return c;
                    }
                }
            }
        }
    }
    roaming
}

fn mtime(p: &Path) -> SystemTime {
    std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or(UNIX_EPOCH)
}

fn millis(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Profile/organisation folder pairs, most recently used first.
pub fn accounts(appdata: &Path) -> Vec<String> {
    let app = app_folder(appdata);
    let mut pairs: Vec<(String, SystemTime)> = Vec::new();
    for sub in APPDATA_FOLDERS {
        let Ok(a) = std::fs::read_dir(app.join(sub)) else { continue };
        for a in a.flatten().filter(|e| e.path().is_dir()) {
            let Ok(o) = std::fs::read_dir(a.path()) else { continue };
            for o in o.flatten().filter(|e| e.path().is_dir()) {
                pairs.push((format!("{}/{}", a.file_name().to_string_lossy(), o.file_name().to_string_lossy()), mtime(&o.path())));
            }
        }
    }
    pairs.sort_by(|a, b| b.1.cmp(&a.1));
    let mut seen = HashSet::new();
    pairs.into_iter().filter(|(k, _)| seen.insert(k.clone())).map(|(k, _)| k).collect()
}

/// Read a file Claude may be writing, without blocking it.
fn read_shared(p: &Path) -> std::io::Result<Vec<u8>> {
    std::fs::read(p)
}

fn walk(dir: &Path, skip: &dyn Fn(&Path, bool) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        if out.len() >= MAX_WALK {
            return;
        }
        let Ok(ft) = e.file_type() else { continue };
        let p = e.path();
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            if !skip(&p, true) {
                walk(&p, skip, out);
            }
        } else if !skip(&p, false) {
            out.push(p);
        }
    }
}

fn rel(p: &Path, base: &Path) -> String {
    p.strip_prefix(base).map(|r| r.to_string_lossy().into_owned()).unwrap_or_default()
}

fn json_string_field(line: &str, field: &str) -> Option<String> {
    thread_local! {
        static CACHE: std::cell::RefCell<HashMap<String, Regex>> = std::cell::RefCell::new(HashMap::new());
    }
    if !line.contains(field) {
        return None;
    }
    let raw = CACHE.with(|c| {
        let mut c = c.borrow_mut();
        let re = c
            .entry(field.to_string())
            .or_insert_with(|| Regex::new(&format!(r#""{}":"((?:[^"\\]|\\.)*)""#, regex::escape(field))).unwrap());
        re.captures(line).and_then(|m| m.get(1)).map(|m| m.as_str().to_string())
    })?;
    serde_json::from_str::<String>(&format!("\"{raw}\"")).ok()
}

/// The working directory a history folder belongs to, from its transcripts.
fn project_cwd(dir: &Path) -> Option<String> {
    use std::io::BufRead;
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jsonl")).collect();
    files.sort_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0));
    for f in files {
        let Ok(file) = std::fs::File::open(&f) else { continue };
        for line in std::io::BufReader::new(file).lines().take(500).map_while(Result::ok) {
            if let Some(c) = json_string_field(&line, "cwd") {
                return Some(c);
            }
        }
    }
    None
}

// Scanning (on the PC the workspace comes from) ----------------------------

fn skip_home_entry(claude_dir: &Path) -> impl Fn(&Path, bool) -> bool + '_ {
    move |p: &Path, is_dir: bool| {
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let top = p.parent() == Some(claude_dir);
        if is_dir {
            (top && SKIP_HOME_FOLDERS.contains(&name.as_str())) || name == "node_modules"
        } else {
            (top && SKIP_HOME_FILES.contains(&name.as_str())) || name.ends_with(".lock")
        }
    }
}

fn skip_rebuildable(p: &Path, is_dir: bool) -> bool {
    is_dir && p.file_name().is_some_and(|n| SKIP_DIRS.iter().any(|s| n.to_string_lossy().eq_ignore_ascii_case(s)))
}

fn copyable_folder(cwd: &str, home: &Path, appdata: &Path) -> bool {
    let p = Path::new(cwd);
    let is_root = p.parent().is_none() || normalize(cwd).len() <= 3;
    p.is_dir() && !is_root && !normalize(cwd).eq_ignore_ascii_case(&normalize(&home.to_string_lossy())) && !is_under(cwd, &appdata.to_string_lossy())
}

pub fn scan() -> Inventory {
    let home = source_home();
    let appdata = source_appdata();
    let claude_dir = home.join(".claude");
    let app = app_folder(&appdata);
    let mut inv = Inventory {
        tool: "claude".into(),
        machine: crate::win::computer_name(),
        home: home.to_string_lossy().into_owned(),
        appdata: appdata.to_string_lossy().into_owned(),
        found: claude_dir.exists(),
        ..Default::default()
    };
    if !inv.found {
        return inv;
    }

    let mut files = Vec::new();
    walk(&claude_dir, &skip_home_entry(&claude_dir), &mut files);
    let mut parts: Vec<(&str, u64, u64)> =
        vec![("Settings and instructions", 0, 0), ("Skills", 0, 0), ("Plugins", 0, 0), ("Memory", 0, 0), ("Session history", 0, 0), ("Claude app sessions", 0, 0)];
    let mut add = |i: usize, p: &Path| {
        parts[i].1 += 1;
        parts[i].2 += std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    };
    for f in &files {
        let r = rel(f, &claude_dir).to_lowercase();
        let i = if r.starts_with("skills\\") {
            1
        } else if r.starts_with("plugins\\") {
            2
        } else if r.starts_with("projects\\") && r.contains("\\memory\\") {
            3
        } else if r.starts_with("projects\\") || r.starts_with("file-history\\") || r == "history.jsonl" {
            4
        } else {
            0
        };
        add(i, f);
    }
    let mut app_files = Vec::new();
    for sub in APPDATA_FOLDERS {
        walk(&app.join(sub), &|_, _| false, &mut app_files);
    }
    for f in &app_files {
        add(5, f);
    }
    inv.parts = parts.into_iter().filter(|p| p.1 > 0).map(|(l, f, b)| Part { label: l.into(), files: f, bytes: b }).collect();

    let scratch_root = app.join("scratch-workspaces");
    if let Ok(rd) = std::fs::read_dir(claude_dir.join("projects")) {
        for e in rd.flatten().filter(|e| e.path().is_dir()) {
            let dir = e.path();
            let folder = e.file_name().to_string_lossy().into_owned();
            let mut sessions = 0;
            let mut bytes = 0;
            let mut last = UNIX_EPOCH;
            let mut all = Vec::new();
            walk(&dir, &|_, _| false, &mut all);
            for f in &all {
                if let Ok(m) = std::fs::metadata(f) {
                    bytes += m.len();
                    last = last.max(m.modified().unwrap_or(UNIX_EPOCH));
                }
                if f.parent() == Some(dir.as_path()) && f.extension().is_some_and(|x| x == "jsonl") {
                    sessions += 1;
                }
            }
            let cwd = project_cwd(&dir);
            let scratch = cwd.as_deref().is_some_and(|c| is_under(c, &scratch_root.to_string_lossy()));
            let mut p = Project {
                key: cwd.clone().unwrap_or_else(|| folder.clone()),
                folder,
                cwd: cwd.clone(),
                sessions,
                session_bytes: bytes,
                last_active: millis(last),
                scratch,
                ..Default::default()
            };
            if let Some(c) = cwd.as_deref().filter(|c| !scratch && copyable_folder(c, &home, &appdata)) {
                let mut pf = Vec::new();
                walk(Path::new(c), &skip_rebuildable, &mut pf);
                p.files_available = true;
                p.file_count = pf.len() as u64;
                p.file_bytes = pf.iter().filter_map(|f| std::fs::metadata(f).ok()).map(|m| m.len()).sum();
            }
            inv.projects.push(p);
        }
    }
    inv.projects.sort_by(|a, b| b.last_active.cmp(&a.last_active));
    inv
}

/// Files to send for `sel`, as (path inside the offer, file on disk), plus
/// the manifest. Generated files go in `scratch_dir`.
pub fn collect(sel: &Selection, scratch_dir: &Path) -> anyhow::Result<(Vec<(String, PathBuf)>, Manifest)> {
    let inv = scan();
    anyhow::ensure!(inv.found, "Claude isn't set up on this PC");
    let home = source_home();
    let appdata = source_appdata();
    let claude_dir = home.join(".claude");
    let app = app_folder(&appdata);
    let choice: HashMap<&str, &super::ProjectChoice> = sel.projects.iter().map(|c| (c.key.as_str(), c)).collect();

    let mut out: Vec<(String, PathBuf)> = Vec::new();
    let skipped_folders: HashSet<String> = inv
        .projects
        .iter()
        .filter(|p| !choice.get(p.key.as_str()).is_some_and(|c| c.sessions))
        .map(|p| p.folder.to_lowercase())
        .collect();

    let mut files = Vec::new();
    walk(&claude_dir, &skip_home_entry(&claude_dir), &mut files);
    for f in files {
        let r = rel(&f, &claude_dir);
        let mut parts = r.split('\\');
        if parts.next().is_some_and(|p| p.eq_ignore_ascii_case("projects")) {
            if let Some(folder) = parts.next() {
                if skipped_folders.contains(&folder.to_lowercase()) {
                    continue;
                }
            }
        }
        out.push((format!("claude\\home\\{r}"), f));
    }
    for sub in APPDATA_FOLDERS {
        let mut af = Vec::new();
        walk(&app.join(sub), &|_, _| false, &mut af);
        for f in af {
            out.push((format!("claude\\appdata\\{}", rel(&f, &app)), f));
        }
    }

    // Only the parts of .claude.json worth moving: MCP servers and per-folder
    // trust and tool permissions. Never sign-in or machine IDs.
    std::fs::create_dir_all(scratch_dir)?;
    if let Ok(bytes) = read_shared(&home.join(".claude.json")) {
        if let Ok(Value::Object(full)) = serde_json::from_slice::<Value>(&bytes) {
            let mut subset = Map::new();
            for k in ["mcpServers", "projects"] {
                if let Some(v) = full.get(k) {
                    subset.insert(k.into(), v.clone());
                }
            }
            let p = scratch_dir.join("claude-json.json");
            std::fs::write(&p, serde_json::to_vec(&Value::Object(subset))?)?;
            out.push(("claude\\claude-json.json".into(), p));
        }
    }

    // Project folders. One nested inside another chosen folder travels with it.
    let chosen: Vec<&Project> = inv
        .projects
        .iter()
        .filter(|p| p.files_available && choice.get(p.key.as_str()).is_some_and(|c| c.files))
        .collect();
    let mut manifest_projects = Vec::new();
    let mut n = 0u32;
    for p in &inv.projects {
        let mut files_index = None;
        if let Some(cwd) = p.cwd.as_deref().filter(|_| chosen.iter().any(|c| c.key == p.key)) {
            let nested = chosen.iter().any(|o| o.key != p.key && o.cwd.as_deref().is_some_and(|oc| is_under(cwd, oc)));
            if !nested {
                let mut pf = Vec::new();
                walk(Path::new(cwd), &skip_rebuildable, &mut pf);
                for f in pf {
                    out.push((format!("files\\{n}\\{}", rel(&f, Path::new(cwd))), f));
                }
                files_index = Some(n);
                n += 1;
            }
        }
        manifest_projects.push(ManifestProject { key: p.key.clone(), folder: p.folder.clone(), cwd: p.cwd.clone(), files: files_index });
    }

    let manifest = Manifest {
        tool: "claude".into(),
        machine: inv.machine,
        home: inv.home,
        appdata: inv.appdata,
        accounts: accounts(&appdata),
        projects: manifest_projects,
    };
    Ok((out, manifest))
}

// Importing (on the receiving PC) -----------------------------------------

/// Merge two JSON values: objects key by key, arrays as a union, and for
/// plain values this PC's (`local`) wins.
fn merge_json(local: Value, incoming: Value) -> Value {
    match (local, incoming) {
        (Value::Object(mut l), Value::Object(i)) => {
            for (k, v) in i {
                let merged = match l.remove(&k) {
                    Some(lv) => merge_json(lv, v),
                    None => v,
                };
                l.insert(k, merged);
            }
            Value::Object(l)
        }
        (Value::Array(l), Value::Array(i)) => {
            let mut seen = HashSet::new();
            Value::Array(l.into_iter().chain(i).filter(|x| seen.insert(x.to_string())).collect())
        }
        (l, _) => l,
    }
}

struct Ctx<'a> {
    journal: &'a mut Journal,
    report: &'a mut Report,
    conflicts: PathBuf,
    machine: String,
    memory_adds: HashMap<PathBuf, Vec<String>>,
}

pub fn apply(staging: &Path, manifest: &Manifest, sel: &Selection, register_all: bool, dry: bool) -> anyhow::Result<Report> {
    let home = target_home();
    let claude_dir = home.join(".claude");
    let appdata = target_appdata();
    let app = app_folder(&appdata);
    let choice: HashMap<&str, &super::ProjectChoice> = sel.projects.iter().map(|c| (c.key.as_str(), c)).collect();

    // Same account on both PCs keeps its folders; otherwise use the one last
    // active here.
    let local_accounts = accounts(&appdata);
    let account_map: HashMap<String, String> = manifest
        .accounts
        .iter()
        .map(|a| {
            let to = if local_accounts.is_empty() || local_accounts.contains(a) { a.clone() } else { local_accounts[0].clone() };
            (a.clone(), to)
        })
        .collect();

    let mut base_pairs: Vec<(String, String)> = Vec::new();
    let src_app = Path::new(&manifest.appdata).join("Claude");
    for (from, to) in &account_map {
        if from != to {
            for sub in ["claude-code-sessions", "local-agent-mode-sessions", "scratch-workspaces"] {
                base_pairs.push((
                    src_app.join(sub).join(from.replace('/', "\\")).to_string_lossy().into_owned(),
                    app.join(sub).join(to.replace('/', "\\")).to_string_lossy().into_owned(),
                ));
            }
        }
    }
    base_pairs.push((manifest.appdata.clone(), appdata.to_string_lossy().into_owned()));
    base_pairs.push((manifest.home.clone(), home.to_string_lossy().into_owned()));

    let mut excluded_folders = HashSet::new();
    let mut excluded_cwds = Vec::new();
    // Claude app scratch folders travel with the app data, so the app-data
    // mapping (including a changed profile folder) decides where they go.
    let src_scratch = src_app.join("scratch-workspaces").to_string_lossy().into_owned();
    let mut project_pairs = Vec::new();
    let mut session_projects = Vec::new();
    for p in &manifest.projects {
        let c = choice.get(p.key.as_str());
        let sessions = c.is_some_and(|c| c.sessions);
        let included = c.is_some_and(|c| c.sessions || c.files);
        if !sessions {
            excluded_folders.insert(p.folder.to_lowercase());
        }
        if !included {
            if let Some(cwd) = &p.cwd {
                excluded_cwds.push(cwd.clone());
            }
            continue;
        }
        let target = normalize(&c.unwrap().target);
        if let Some(cwd) = p.cwd.as_ref().filter(|c| !is_under(c, &src_scratch)) {
            if !target.is_empty() {
                project_pairs.push((cwd.clone(), target.clone()));
            }
        }
        if sessions {
            session_projects.push(p);
        }
    }
    let all_pairs: Vec<(String, String)> = project_pairs.iter().chain(base_pairs.iter()).cloned().collect();
    let rewriter = Rewriter::new(&all_pairs);

    // History folders are named after the (rewritten) working directory, so
    // they match the paths inside the moved sessions. Windows caps a folder
    // name at 255 characters; past Claude's own 200 keep the old name.
    let mut folder_map: HashMap<String, String> = HashMap::new();
    let mut long_names = Vec::new();
    for p in session_projects {
        let mut to = match (&p.cwd, &rewriter) {
            (Some(cwd), Some(rw)) => project_folder_name(&rw.apply(cwd)),
            _ => map_folder_name(&p.folder, &base_pairs),
        };
        if to.len() > 200 {
            long_names.push(p.key.clone());
            to = p.folder.clone();
        }
        folder_map.insert(p.folder.to_lowercase(), to);
    }

    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let backup = crate::config::data_dir().join("ai-backups").join(&stamp);
    let mut journal = Journal::new(backup.clone(), dry)?;
    let mut report = Report::default();
    for k in long_names {
        report.notes.push(format!("The new path for {k} is too long for Claude's history folder, so its sessions keep their old folder name. Pick a shorter destination if they don't resume."));
    }
    let machine: String = manifest.machine.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    let mut ctx = Ctx {
        journal: &mut journal,
        report: &mut report,
        conflicts: backup.join("conflicts"),
        machine: if machine.is_empty() { "other-pc".into() } else { machine },
        memory_adds: HashMap::new(),
    };

    // 1. Project folders. Never replace a file that is newer here.
    for p in &manifest.projects {
        let (Some(i), Some(c)) = (p.files, choice.get(p.key.as_str())) else { continue };
        if !c.files || normalize(&c.target).is_empty() {
            continue;
        }
        let src = staging.join("files").join(i.to_string());
        let mut fs = Vec::new();
        walk(&src, &|_, _| false, &mut fs);
        let target = PathBuf::from(normalize(&c.target));
        for f in fs {
            let dest = target.join(rel(&f, &src));
            if dest.exists() && mtime(&dest) >= mtime(&f) {
                ctx.report.project_files_kept += 1;
                continue;
            }
            ctx.journal.copy(&f, &dest)?;
            ctx.report.project_files += 1;
        }
    }

    // 2. Claude data.
    let claude_src = staging.join("claude");
    let mut entries = Vec::new();
    walk(&claude_src, &|_, _| false, &mut entries);
    for f in entries {
        let r = rel(&f, &claude_src);
        let parts: Vec<&str> = r.split('\\').collect();
        let mut kind = "data";
        let dest: PathBuf = if r.eq_ignore_ascii_case("claude-json.json") {
            kind = "claude-json";
            home.join(".claude.json")
        } else if parts[0] == "home" && parts.len() >= 2 {
            if parts.len() == 2 && parts[1].eq_ignore_ascii_case("settings.json") {
                kind = "json-merge";
                claude_dir.join("settings.json")
            } else if parts[1].eq_ignore_ascii_case("projects") && parts.len() >= 4 {
                let folder = parts[2].to_lowercase();
                if excluded_folders.contains(&folder) {
                    ctx.report.skipped += 1;
                    continue;
                }
                let new_folder = folder_map.get(&folder).cloned().unwrap_or_else(|| parts[2].to_string());
                claude_dir.join("projects").join(new_folder).join(parts[3..].join("\\"))
            } else {
                if parts[1].eq_ignore_ascii_case("plugins") && parts.len() == 3 && parts[2].to_lowercase().ends_with(".json") {
                    kind = "json-merge";
                }
                claude_dir.join(parts[1..].join("\\"))
            }
        } else if parts[0] == "appdata" && parts.len() >= 2 {
            let mut p: Vec<String> = parts[1..].iter().map(|s| s.to_string()).collect();
            if p.len() >= 4 {
                if let Some(to) = account_map.get(&format!("{}/{}", p[1], p[2])) {
                    let mut t = to.split('/');
                    p[1] = t.next().unwrap_or(&p[1]).to_string();
                    p[2] = t.next().unwrap_or(&p[2]).to_string();
                }
            }
            app.join(p.join("\\"))
        } else {
            continue;
        };
        import_file(&mut ctx, &f, &r, &dest, kind, rewriter.as_ref(), &excluded_cwds)?;
    }

    // 3. Memory indexes get a line for each memory kept in two versions.
    for (dir, lines) in std::mem::take(&mut ctx.memory_adds) {
        let index = dir.join("MEMORY.md");
        let current = std::fs::read_to_string(&index).unwrap_or_default();
        let add: Vec<&String> = lines.iter().filter(|l| !current.contains(l.as_str())).collect();
        if !add.is_empty() {
            let text = format!("{}\n{}\n", current.trim_end(), add.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"));
            ctx.journal.write(&index, text.as_bytes(), None)?;
        }
    }

    // 4. Make sessions show up in the Claude app's list.
    let dirs: Vec<PathBuf> = if register_all {
        std::fs::read_dir(claude_dir.join("projects")).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default()
    } else {
        folder_map.values().map(|f| claude_dir.join("projects").join(f)).collect()
    };
    let registered = register_sessions(&dirs, &app, ctx.journal, dry, &mut ctx.report.notes)?;
    ctx.report.sessions_registered = registered;

    // 5. Git worktrees store absolute paths.
    if !dry {
        for p in &manifest.projects {
            if let (Some(_), Some(c)) = (p.files, choice.get(p.key.as_str())) {
                if c.files {
                    repair_worktrees(Path::new(&normalize(&c.target)), &mut ctx.report.notes);
                }
            }
        }
    }

    let changed = ctx.journal.changed();
    journal.save()?;
    if !dry && changed > 0 {
        report.backup = Some(backup.to_string_lossy().into_owned());
    }
    Ok(report)
}

fn import_file(ctx: &mut Ctx, src: &Path, entry: &str, dest: &Path, kind: &str, rw: Option<&Rewriter>, excluded_cwds: &[String]) -> anyhow::Result<()> {
    let mut bytes = read_shared(src)?;
    let ext = dest.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let is_text = TEXT_EXTS.contains(&ext.as_str());
    let is_session_entry = name.starts_with("local_") && ext == "json";
    let modified = std::fs::metadata(src).and_then(|m| m.modified()).ok();

    let mut text = None;
    if is_text {
        let original = String::from_utf8_lossy(&bytes).into_owned();
        // App session entries for projects left behind stay behind too.
        if is_session_entry && !excluded_cwds.is_empty() {
            if let Some(cwd) = json_string_field(&original, "cwd") {
                if excluded_cwds.iter().any(|e| is_under(&cwd, e)) {
                    ctx.report.skipped += 1;
                    return Ok(());
                }
            }
        }
        let t = rw.map_or(original.clone(), |r| r.apply(&original));
        bytes = t.clone().into_bytes();
        text = Some(t);
    }

    if kind == "claude-json" || (kind == "json-merge" && dest.exists()) {
        let incoming: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        if !dest.exists() {
            ctx.journal.write(dest, &serde_json::to_vec_pretty(&incoming)?, None)?;
            ctx.report.added += 1;
            return Ok(());
        }
        let local: Value = serde_json::from_slice(&read_shared(dest)?).unwrap_or(Value::Object(Map::new()));
        let before = local.to_string();
        let merged = merge_json(local, incoming);
        if merged.to_string() == before {
            ctx.report.unchanged += 1;
        } else {
            let out = if kind == "claude-json" { serde_json::to_vec(&merged)? } else { serde_json::to_vec_pretty(&merged)? };
            ctx.journal.write(dest, &out, None)?;
            ctx.report.merged += 1;
        }
        return Ok(());
    }

    if !dest.exists() {
        ctx.journal.write(dest, &bytes, modified)?;
        ctx.report.added += 1;
        return Ok(());
    }

    let local = read_shared(dest)?;
    if local == bytes {
        ctx.report.unchanged += 1;
        return Ok(());
    }
    let local_text = String::from_utf8_lossy(&local).into_owned();
    let text = text.unwrap_or_default();

    if is_text && ext == "jsonl" {
        // Transcripts only grow, so the copy that contains the other is newer.
        if text.starts_with(&local_text) {
            ctx.journal.write(dest, &bytes, modified)?;
            ctx.report.updated += 1;
        } else if local_text.starts_with(&text) {
            ctx.report.kept_local += 1;
        } else {
            conflict(ctx, entry, &bytes)?;
        }
    } else if is_text && name == "MEMORY.md" {
        let have: HashSet<&str> = local_text.lines().map(str::trim).collect();
        let link = Regex::new(r"\]\(([^)]+)\)").unwrap();
        let add: Vec<&str> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#') && !have.contains(l))
            .filter(|l| !link.captures(l).is_some_and(|c| local_text.contains(&format!("]({})", &c[1]))))
            .collect();
        if add.is_empty() {
            ctx.report.unchanged += 1;
        } else {
            let merged = format!("{}\n{}\n", local_text.trim_end(), add.join("\n"));
            ctx.journal.write(dest, merged.as_bytes(), None)?;
            ctx.report.merged += 1;
        }
    } else if is_text && ext == "md" && dest.components().any(|c| c.as_os_str().eq_ignore_ascii_case("memory")) {
        // Keep both versions of a memory and point the index at this one.
        let stem = dest.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let alt = dest.with_file_name(format!("{stem}.from-{}.md", ctx.machine));
        if alt.exists() && read_shared(&alt)? == bytes {
            ctx.report.unchanged += 1;
        } else {
            ctx.journal.write(&alt, &bytes, modified)?;
            let dir = dest.parent().unwrap_or(Path::new("")).to_path_buf();
            ctx.memory_adds.entry(dir).or_default().push(format!(
                "- [{stem} (from {m})]({f}): differs from the local copy; reconcile and delete one",
                m = ctx.machine,
                f = alt.file_name().unwrap().to_string_lossy()
            ));
            ctx.report.merged += 1;
            ctx.report.notes.push(format!("Kept both versions of memory {}", alt.display()));
        }
    } else if is_session_entry {
        let num = |s: &str| json_string_field(s, "lastActivityAt").or_else(|| Regex::new(r#""lastActivityAt":(\d+)"#).unwrap().captures(s).map(|c| c[1].to_string()));
        let (l, i) = (num(&local_text).and_then(|v| v.parse::<u64>().ok()), num(&text).and_then(|v| v.parse::<u64>().ok()));
        if i.is_some() && (l.is_none() || i > l) {
            ctx.journal.write(dest, &bytes, modified)?;
            ctx.report.updated += 1;
        } else {
            ctx.report.kept_local += 1;
        }
    } else {
        conflict(ctx, entry, &bytes)?;
    }
    Ok(())
}

fn conflict(ctx: &mut Ctx, entry: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let saved = ctx.conflicts.join(entry);
    ctx.journal.write(&saved, bytes, None)?;
    ctx.report.conflicts += 1;
    ctx.report.notes.push(format!("Kept this PC's copy of {entry}; the other version is in the backup's conflicts folder"));
    Ok(())
}

// The Claude app's session list ---------------------------------------------

fn known_session_ids(root: &Path) -> (HashSet<String>, Vec<PathBuf>) {
    let mut entries = Vec::new();
    walk(root, &|_, _| false, &mut entries);
    entries.retain(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("local_")) && p.extension().is_some_and(|e| e == "json"));
    let id = Regex::new(r"[0-9a-f-]{36}").unwrap();
    let field = Regex::new(r#""(?:cliSessionId|priorCliSessionIds)":(?:"([0-9a-f-]{36})"|\[([^\]]*)\])"#).unwrap();
    let mut known = HashSet::new();
    for e in &entries {
        let Ok(t) = std::fs::read_to_string(e) else { continue };
        for c in field.captures_iter(&t) {
            if let Some(m) = c.get(1) {
                known.insert(m.as_str().to_lowercase());
            }
            if let Some(m) = c.get(2) {
                for i in id.find_iter(m.as_str()) {
                    known.insert(i.as_str().to_lowercase());
                }
            }
        }
    }
    (known, entries)
}

/// Transcripts in `dirs` with no entry in the app's list.
fn unlisted(dirs: &[PathBuf], known: &HashSet<String>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "jsonl") {
                continue;
            }
            let id = p.file_stem().unwrap().to_string_lossy().to_lowercase();
            if known.contains(&id) || d.join(format!("{id}.desktop-released.json")).exists() {
                continue;
            }
            out.push(p);
        }
    }
    out
}

fn first_prompt(v: &Value) -> Option<String> {
    let content = v.get("message")?.get("content")?;
    let s = match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().find_map(|i| (i.get("type")? == "text").then(|| i.get("text")?.as_str().map(String::from)).flatten())?,
        _ => return None,
    };
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    // Skip slash-command and system wrappers.
    (!s.is_empty() && !s.starts_with('<')).then_some(s)
}

/// Create app session entries for transcripts in `dirs` that have none,
/// titled from the transcript. Sessions deleted in the app stay deleted.
fn register_sessions(dirs: &[PathBuf], app: &Path, journal: &mut Journal, dry: bool, notes: &mut Vec<String>) -> anyhow::Result<u32> {
    use std::io::BufRead;
    let root = app.join("claude-code-sessions");
    let (known, existing) = known_session_ids(&root);
    let todo = unlisted(dirs, &known);
    if todo.is_empty() {
        return Ok(0);
    }

    // New entries go next to the most recently used ones and copy their
    // model and mode.
    let template = existing.iter().max_by_key(|p| mtime(p));
    let dir = match template {
        Some(t) => t.parent().unwrap().to_path_buf(),
        None => match accounts(&target_appdata()).first() {
            Some(a) => root.join(a.replace('/', "\\")),
            None => {
                notes.push("Open the Claude app once on this PC and sign in, then use \"Show every session\" so moved sessions appear in its list.".into());
                return Ok(0);
            }
        },
    };
    let defaults: Map<String, Value> = template
        .and_then(|t| std::fs::read(t).ok())
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|o| o.into_iter().filter(|(k, _)| ["model", "effort", "permissionMode"].contains(&k.as_str())).collect())
        .unwrap_or_default();

    let ts = Regex::new(r#""timestamp":"([^"]+)""#).unwrap();
    let mut added = 0;
    for f in todo {
        let id = f.file_stem().unwrap().to_string_lossy().into_owned();
        let Ok(file) = std::fs::File::open(&f) else { continue };
        let (mut cwd, mut first, mut last, mut custom, mut ai, mut prompt) = (None, None, None, None, None, None);
        for line in std::io::BufReader::new(file).lines().map_while(Result::ok) {
            if let Some(c) = ts.captures(&line) {
                let t = c[1].to_string();
                if first.is_none() {
                    first = Some(t.clone());
                }
                last = Some(t);
            }
            if cwd.is_none() {
                cwd = json_string_field(&line, "cwd");
            }
            if line.starts_with(r#"{"type":"custom-title""#) {
                custom = json_string_field(&line, "customTitle");
            } else if line.starts_with(r#"{"type":"ai-title""#) {
                ai = json_string_field(&line, "aiTitle");
            } else if prompt.is_none() && line.contains(r#""type":"user""#) && !line.contains(r#""isMeta":true"#) {
                prompt = serde_json::from_str::<Value>(&line).ok().as_ref().and_then(first_prompt);
            }
        }
        let (Some(cwd), Some(first)) = (cwd, first) else { continue };
        let ms = |s: &str| chrono::DateTime::parse_from_rfc3339(s).map(|d| d.timestamp_millis()).unwrap_or(0);
        let created = ms(&first);
        let active = ms(last.as_deref().unwrap_or(&first));
        let mut title = custom.clone().or(ai).or(prompt).unwrap_or_else(|| id.clone());
        if title.chars().count() > 80 {
            title = title.chars().take(77).collect::<String>() + "...";
        }
        let session_id = format!("local_{}", uuid::Uuid::new_v4());
        let mut entry = json!({
            "sessionId": session_id,
            "cliSessionId": id,
            "cwd": cwd,
            "originCwd": cwd,
            "gitAnchorsFolderRealpath": cwd,
            "createdAt": created,
            "lastActivityAt": active,
            "lastFocusedAt": active,
            "isArchived": false,
            "title": title,
            "titleSource": if custom.is_some() { "user" } else { "auto" },
        });
        for (k, v) in &defaults {
            entry[k] = v.clone();
        }
        if !dry {
            journal.write(&dir.join(format!("{session_id}.json")), &serde_json::to_vec(&entry)?, None)?;
        }
        added += 1;
    }
    Ok(added)
}

/// How many sessions on this PC are missing from the Claude app's list.
pub fn unlisted_count() -> u32 {
    let claude_dir = target_home().join(".claude").join("projects");
    let app = app_folder(&target_appdata());
    let (known, _) = known_session_ids(&app.join("claude-code-sessions"));
    let dirs: Vec<PathBuf> = std::fs::read_dir(claude_dir).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default();
    unlisted(&dirs, &known).len() as u32
}

/// Add every session on this PC to the Claude app's list. Undoable.
pub fn register_all_local() -> anyhow::Result<Report> {
    let claude_dir = target_home().join(".claude").join("projects");
    let app = app_folder(&target_appdata());
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let backup = crate::config::data_dir().join("ai-backups").join(format!("{stamp}-sessions"));
    let mut journal = Journal::new(backup.clone(), false)?;
    let dirs: Vec<PathBuf> = std::fs::read_dir(claude_dir).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default();
    let mut report = Report::default();
    report.sessions_registered = register_sessions(&dirs, &app, &mut journal, false, &mut report.notes)?;
    journal.save()?;
    if report.sessions_registered > 0 {
        report.backup = Some(backup.to_string_lossy().into_owned());
    }
    Ok(report)
}

fn git_exe() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("PATH").and_then(|path| std::env::split_paths(&path).map(|d| d.join("git.exe")).find(|p| p.exists())) {
        return Some(p);
    }
    let gd = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("GitHubDesktop");
    let mut apps: Vec<PathBuf> = std::fs::read_dir(gd).ok()?.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("app-"))).collect();
    apps.sort();
    apps.into_iter().rev().map(|a| a.join("resources").join("app").join("git").join("cmd").join("git.exe")).find(|p| p.exists())
}

fn repair_worktrees(dir: &Path, notes: &mut Vec<String>) {
    let mut repos = Vec::new();
    if dir.join(".git").is_dir() {
        repos.push(dir.to_path_buf());
    }
    for d in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if d.path().join(".git").is_dir() {
            repos.push(d.path());
        }
    }
    for repo in repos {
        let wt_root = repo.join(".claude").join("worktrees");
        let worktrees: Vec<PathBuf> = std::fs::read_dir(&wt_root).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect()).unwrap_or_default();
        if worktrees.is_empty() && !repo.join(".git").join("worktrees").exists() {
            continue;
        }
        match git_exe() {
            Some(git) => {
                use std::os::windows::process::CommandExt;
                let ok = std::process::Command::new(git)
                    .arg("-C")
                    .arg(&repo)
                    .args(["worktree", "repair"])
                    .args(&worktrees)
                    .creation_flags(0x0800_0000)
                    .status()
                    .is_ok_and(|s| s.success());
                notes.push(format!("{} git worktrees in {}", if ok { "Repaired" } else { "Could not repair" }, repo.display()));
            }
            None => notes.push(format!("Install git, then run \"git worktree repair\" in {}", repo.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_json_with_local_winning() {
        let local = json!({"a": 1, "list": [1, 2], "obj": {"x": "here"}});
        let incoming = json!({"a": 2, "b": 3, "list": [2, 3], "obj": {"x": "there", "y": true}});
        assert_eq!(merge_json(local, incoming), json!({"a": 1, "list": [1, 2, 3], "obj": {"x": "here", "y": true}, "b": 3}));
    }

    #[test]
    fn reads_escaped_fields() {
        let line = r#"{"type":"user","cwd":"D:\\Claude Does Things","x":1}"#;
        assert_eq!(json_string_field(line, "cwd").as_deref(), Some(r"D:\Claude Does Things"));
    }

    #[test]
    fn prompt_titles() {
        let v = json!({"message": {"content": [{"type": "text", "text": "  Fix   the\nbug "}]}});
        assert_eq!(first_prompt(&v).as_deref(), Some("Fix the bug"));
        let v = json!({"message": {"content": "<command-name>/clear</command-name>"}});
        assert_eq!(first_prompt(&v), None);
    }
}
