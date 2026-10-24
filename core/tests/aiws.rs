//! Adding command-line Claude sessions to the Claude app's session list.

use std::path::Path;

#[test]
fn registers_unlisted_sessions_and_undoes() {
    let root = std::env::temp_dir().join(format!("seam-aiws-{}", rand_suffix()));
    let home = root.join("home");
    let appdata = root.join("appdata");
    let project = home.join(".claude").join("projects").join("D--Work-App");
    let listed_dir = appdata.join("Claude").join("claude-code-sessions").join("prof").join("org");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&listed_dir).unwrap();

    // One session the app already lists, one it doesn't.
    let listed = "11111111-1111-1111-1111-111111111111";
    let unlisted = "22222222-2222-2222-2222-222222222222";
    std::fs::write(
        listed_dir.join("local_aaaa.json"),
        format!(r#"{{"sessionId":"local_aaaa","cliSessionId":"{listed}","cwd":"D:\\Work\\App","model":"claude-opus-5-5","effort":"high","permissionMode":"auto"}}"#),
    )
    .unwrap();
    std::fs::write(project.join(format!("{listed}.jsonl")), r#"{"type":"user","cwd":"D:\\Work\\App","timestamp":"2026-10-01T10:00:00Z"}"#).unwrap();
    std::fs::write(
        project.join(format!("{unlisted}.jsonl")),
        [
            r#"{"type":"summary","timestamp":"2026-10-02T09:00:00.000Z"}"#,
            r#"{"type":"user","isMeta":true,"cwd":"D:\\Work\\App","message":{"content":"<command-name>/init</command-name>"},"timestamp":"2026-10-02T09:00:01.000Z"}"#,
            r#"{"type":"user","cwd":"D:\\Work\\App","message":{"content":[{"type":"text","text":"Add a dark   mode toggle"}]},"timestamp":"2026-10-02T09:00:02.000Z"}"#,
            r#"{"type":"assistant","timestamp":"2026-10-02T09:30:00.000Z"}"#,
        ]
        .join("\n"),
    )
    .unwrap();

    std::env::set_var("SEAM_AI_TARGET_HOME", &home);
    std::env::set_var("SEAM_AI_TARGET_APPDATA", &appdata);
    std::env::set_var("SEAM_PROFILE", format!("test-{}", rand_suffix()));

    assert_eq!(seam_core::aiws::claude::unlisted_count(), 1);
    let report = seam_core::aiws::claude::register_all_local().unwrap();
    assert_eq!(report.sessions_registered, 1);
    assert_eq!(seam_core::aiws::claude::unlisted_count(), 0);

    let new_entry = std::fs::read_dir(&listed_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().unwrap() != "local_aaaa.json")
        .expect("a new entry");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&new_entry).unwrap()).unwrap();
    assert_eq!(v["cliSessionId"], unlisted);
    assert_eq!(v["cwd"], r"D:\Work\App");
    assert_eq!(v["title"], "Add a dark mode toggle");
    assert_eq!(v["titleSource"], "auto");
    assert_eq!(v["model"], "claude-opus-5-5");
    assert_eq!(v["permissionMode"], "auto");
    assert_eq!(v["isArchived"], false);
    assert_eq!(v["createdAt"], 1790931600000i64); // 2026-10-02 09:00 UTC
    assert_eq!(v["lastActivityAt"], 1790933400000i64); // 09:30 UTC

    seam_core::aiws::journal::undo(Path::new(report.backup.as_ref().unwrap())).unwrap();
    assert!(!new_entry.exists());
    assert_eq!(seam_core::aiws::claude::unlisted_count(), 1);
    let _ = std::fs::remove_dir_all(&root);
}

fn rand_suffix() -> u32 {
    std::process::id() ^ (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos())
}
