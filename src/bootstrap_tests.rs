use super::*;
use std::collections::HashMap;
use tempfile::TempDir;

fn setup_test_db() -> (TempDir, HcomDb) {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("test.db");
    let db = HcomDb::open_at(&db_path).unwrap();
    (tmp, db)
}

/// Insert a minimal instance for testing.
fn insert_instance(db: &HcomDb, name: &str, status: &str, tool: &str, tag: Option<&str>) {
    let mut data = HashMap::new();
    data.insert("name".to_string(), serde_json::json!(name));
    data.insert("status".to_string(), serde_json::json!(status));
    data.insert("tool".to_string(), serde_json::json!(tool));
    data.insert(
        "status_time".to_string(),
        serde_json::json!(crate::shared::time::now_epoch_i64() as u64),
    );
    data.insert("created_at".to_string(), serde_json::json!(1000.0));
    if let Some(t) = tag {
        data.insert("tag".to_string(), serde_json::json!(t));
    }
    db.save_instance(&data).unwrap();
}

#[test]
fn test_get_scripts_bundled_only() {
    let tmp = TempDir::new().unwrap();
    let result = get_scripts(tmp.path());
    // Should list all bundled scripts
    assert!(result.starts_with("Scripts: "));
    assert!(result.contains("confess"));
    assert!(result.contains("debate"));
    assert!(result.contains("fatcow"));
}

#[test]
fn test_get_scripts_with_user_scripts() {
    let tmp = TempDir::new().unwrap();
    let scripts = tmp.path().join("scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::write(scripts.join("custom.sh"), "#!/bin/bash").unwrap();
    fs::write(scripts.join("_hidden.py"), "# skip").unwrap();
    fs::write(scripts.join("other.py"), "# include").unwrap();

    let result = get_scripts(tmp.path());
    assert!(result.contains("custom"));
    assert!(result.contains("other"));
    assert!(!result.contains("_hidden"));
}

#[test]
fn test_get_active_instances_empty_db() {
    let (_tmp, db) = setup_test_db();
    let result = get_active_instances(&db, "test");
    assert_eq!(result, "");
}

#[test]
fn test_get_active_instances_with_instances() {
    let (_tmp, db) = setup_test_db();
    insert_instance(&db, "luna", "active", "claude", None);

    let result = get_active_instances(&db, "other");
    assert!(result.contains("luna"));
    assert!(result.contains("Active (snapshot)"));
}

#[test]
fn test_get_active_instances_excludes_self() {
    let (_tmp, db) = setup_test_db();
    insert_instance(&db, "luna", "active", "claude", None);

    let result = get_active_instances(&db, "luna");
    assert_eq!(result, "");
}

#[test]
fn test_get_active_instances_grouped_by_tool() {
    let (_tmp, db) = setup_test_db();
    insert_instance(&db, "luna", "active", "claude", None);
    insert_instance(&db, "nova", "active", "claude", None);
    insert_instance(&db, "kira", "active", "codex", None);

    let result = get_active_instances(&db, "other");
    assert!(result.contains("claude: "));
    assert!(result.contains("codex: "));
    assert!(result.contains("luna"));
    assert!(result.contains("nova"));
    assert!(result.contains("kira"));
}

#[test]
fn test_get_bootstrap_claude() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "luna",
        "claude",
        false,
        true,
        "",
        "",
        false,
        None,
    );

    assert!(result.contains("<hcom_system_context>"));
    assert!(result.contains("[HCOM SESSION]"));
    assert!(result.contains("Your name: luna"));
    assert!(result.contains("--name luna"));
    assert!(result.contains("SUBAGENTS")); // Claude-specific section
    assert!(result.contains("Messages instantly and automatically arrive")); // Auto delivery
    assert!(!result.contains("Headless mode")); // Not headless
    assert!(result.contains("</hcom_system_context>"));
}

#[test]
fn test_get_bootstrap_codex_launched() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "nova",
        "codex",
        false,
        true,
        "",
        "",
        false,
        None,
    );

    assert!(result.contains("Messages instantly and automatically arrive"));
    assert!(!result.contains("SUBAGENTS")); // Not claude
}

#[test]
fn test_get_bootstrap_adhoc() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "kira",
        "adhoc",
        false,
        false,
        "",
        "",
        false,
        None,
    );

    assert!(result.contains("Messages do NOT arrive automatically"));
    assert!(result.contains("CONNECTED MODE"));
}

#[test]
fn test_get_bootstrap_with_tag() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "luna",
        "claude",
        false,
        true,
        "",
        "p0c",
        false,
        None,
    );

    assert!(result.contains("tagged 'p0c'"));
    if cfg!(windows) {
        assert!(result.contains("send '@p0c-'"));
    } else {
        assert!(result.contains("send @p0c-"));
    }
}

#[test]
fn test_get_bootstrap_with_relay() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "luna",
        "claude",
        false,
        true,
        "",
        "",
        true,
        None,
    );

    assert!(result.contains("Remote agents have suffix"));
}

#[test]
fn test_get_bootstrap_headless() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "luna",
        "claude",
        true,
        true,
        "",
        "",
        false,
        None,
    );

    assert!(result.contains("Headless mode"));
}

#[test]
fn test_get_bootstrap_with_notes() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "luna",
        "claude",
        false,
        true,
        "Remember to use bun",
        "",
        false,
        None,
    );

    assert!(result.contains("## NOTES"));
    assert!(result.contains("Remember to use bun"));
}

#[test]
fn test_get_subagent_bootstrap() {
    let result = get_subagent_bootstrap("luna_reviewer_1", "luna");

    assert!(result.contains("<hcom>"));
    assert!(result.contains("Your name: luna_reviewer_1"));
    assert!(result.contains("Your parent: luna"));
    assert!(result.contains("--name luna_reviewer_1"));
    assert!(result.contains(SENDER));
    assert!(result.contains("</hcom>"));
    if cfg!(windows) {
        assert!(result.contains("send '@luna' --intent inform"));
        assert!(result.contains("send '@name(s)'"));
    } else {
        assert!(result.contains("send @luna --intent inform"));
        assert!(result.contains("send @name(s)"));
    }
}

#[test]
fn test_bootstrap_quotes_send_recipients_on_windows() {
    let (tmp, db) = setup_test_db();
    let result = get_bootstrap(
        &db,
        tmp.path(),
        "luna",
        "claude",
        false,
        true,
        "",
        "team",
        false,
        None,
    );

    if cfg!(windows) {
        assert!(result.contains("send '@name(s)'"));
        assert!(result.contains("send '@luna' '@nova'"));
        assert!(result.contains("send '@team-' -- msg"));
    } else {
        assert!(result.contains("send @name(s)"));
        assert!(result.contains("send @luna @nova"));
        assert!(result.contains("send @team- -- msg"));
    }
}

#[test]
fn test_render_template_replaces_all() {
    let ctx = BootstrapContext {
        instance_name: "luna".to_string(),
        display_name: "p0c-luna".to_string(),
        tag: "p0c".to_string(),
        relay_enabled: false,
        hcom_cmd: "hcom".to_string(),
        is_launched: true,
        is_headless: false,
        active_instances: String::new(),
        scripts: "Scripts: clone".to_string(),
        launch_tools: launch_tool_names(),
        notes: String::new(),
    };

    let result = render_template("Name: {display_name}, Instance: {instance_name}", &ctx);
    assert_eq!(result, "Name: p0c-luna, Instance: luna");
}

#[test]
fn test_get_bootstrap_antigravity_launched_gets_auto_delivery() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "bono",
        "antigravity",
        false,
        true,
        "",
        "",
        false,
        None,
    );

    // agy uses the same auto-delivery section as the other managed tools.
    assert!(result.contains("Messages instantly and automatically arrive"));
    assert!(result.contains("hcom <command> --help"));
}

#[test]
fn test_get_bootstrap_omp_launched_gets_auto_delivery() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "nova",
        "omp",
        false,
        true,
        "",
        "",
        false,
        None,
    );

    assert!(result.contains("Messages instantly and automatically arrive"));
    assert!(!result.contains("Messages do NOT arrive automatically"));
}

#[test]
fn test_antigravity_delivery_action_guards_against_ack_only_stall() {
    // The per-turn preamble must tell agy that an ACK alone doesn't finish a
    // request and that no turn is auto-created to resume after it goes idle.
    assert!(ANTIGRAVITY_DELIVERY_ACTION.contains("HCOM MESSAGE"));
    assert!(ANTIGRAVITY_DELIVERY_ACTION.contains("ACK alone does not complete"));
    assert!(ANTIGRAVITY_DELIVERY_ACTION.contains("no turn is auto-created"));
}

#[test]
fn test_get_bootstrap_gemini_launched_gets_auto_delivery() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "nova",
        "gemini",
        false,
        true,
        "",
        "",
        false,
        None,
    );

    assert!(result.contains("Messages instantly and automatically arrive"));
}

#[test]
fn test_get_bootstrap_gemini_not_launched_gets_adhoc_delivery() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "nova",
        "gemini",
        false,
        false,
        "",
        "",
        false,
        None,
    );

    assert!(result.contains("Messages do NOT arrive automatically"));
}

#[test]
fn test_get_bootstrap_opencode_launched_gets_auto_delivery() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "nova",
        "opencode",
        false,
        true, // is_launched
        "",
        "",
        false,
        None,
    );

    assert!(result.contains("Messages instantly and automatically arrive"));
}

#[test]
fn test_get_bootstrap_opencode_vanilla_gets_adhoc_delivery() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "nova",
        "opencode",
        false,
        false, // not launched
        "",
        "",
        false,
        None,
    );

    assert!(result.contains("Messages do NOT arrive automatically"));
}

#[test]
fn test_get_bootstrap_kilo_launched_gets_auto_delivery() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "nova",
        "kilo",
        false,
        true,
        "",
        "",
        false,
        None,
    );

    assert!(result.contains("Messages instantly and automatically arrive"));
}

#[test]
fn test_get_bootstrap_cursor_launched_gets_hook_primary_delivery() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "nova",
        "cursor",
        false,
        true,
        "",
        "",
        false,
        None,
    );

    assert!(result.contains("CURSOR DELIVERY"));
    assert!(result.contains("wake trigger"));
    assert!(result.contains("End your turn immediately"));
}

#[test]
fn test_get_bootstrap_background_is_headless() {
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "luna",
        "claude",
        false,
        true,
        "",
        "",
        false,
        Some("agent.log"),
    );

    assert!(result.contains("Headless mode"));
}

#[test]
fn test_get_bootstrap_instance_tag_overrides_config() {
    let (tmp, db) = setup_test_db();
    insert_instance(&db, "luna", "active", "claude", Some("team-a"));

    // Config tag is "team-b" but instance has "team-a"
    let result = get_bootstrap(
        &db,
        tmp.path(),
        "luna",
        "claude",
        false,
        true,
        "",
        "team-b",
        false,
        None,
    );

    assert!(result.contains("tagged 'team-a'"));
    assert!(!result.contains("team-b"));
}

#[test]
fn test_get_bootstrap_display_name_with_tag() {
    let (tmp, db) = setup_test_db();
    insert_instance(&db, "luna", "active", "claude", Some("p0c"));

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "luna",
        "claude",
        false,
        true,
        "",
        "",
        false,
        None,
    );

    assert!(result.contains("Your name: p0c-luna"));
}

#[test]
fn test_get_bootstrap_unescapes_double_braces() {
    // Template uses {{name}} {{status}} (escaped braces).
    // render_template unescapes to {name} {status} in final output.
    let (tmp, db) = setup_test_db();

    let result = get_bootstrap(
        &db,
        tmp.path(),
        "luna",
        "claude",
        false,
        true,
        "",
        "",
        false,
        None,
    );

    assert!(result.contains("{name}"));
    assert!(!result.contains("{{name}}"));
}

/// Catch drift between scripts::SCRIPTS const and actual files in scripts/bundled/.
#[test]
fn test_bundled_scripts_matches_directory() {
    use crate::scripts;

    // Resolve the bundled scripts directory relative to the crate root.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let bundled_dir = std::path::Path::new(manifest_dir).join("src/scripts/bundled");

    if !bundled_dir.exists() {
        // In CI or worktrees, the scripts source may not be present — skip gracefully.
        return;
    }

    let mut actual: Vec<String> = Vec::new();
    for entry in fs::read_dir(&bundled_dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".sh") && !name.starts_with('_') {
            actual.push(name.trim_end_matches(".sh").to_string());
        }
    }
    actual.sort();

    let mut expected: Vec<String> = scripts::SCRIPTS
        .iter()
        .map(|(name, _)| name.to_string())
        .collect();
    expected.sort();

    assert_eq!(
        expected, actual,
        "scripts::SCRIPTS const is out of sync with scripts/bundled/. \
         Expected: {:?}, Actual: {:?}",
        expected, actual
    );
}
