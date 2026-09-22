use crate::hooks::test_helpers::EnvGuard;
use crate::instance_binding::EnvVarGuard;
use serde_json::Value;
use serial_test::serial;
use std::path::PathBuf;

/// Isolated env for verify tests: a single `home` dir so `HCOM_DIR`'s
/// parent (what `claude_config_dir`/`tool_config_root` resolve against)
/// is the same directory tests write fixtures into. Clears
/// `GEMINI_CLI_HOME` deliberately — `agy_plugin_dir()` reads it, and a
/// developer's real env must not leak into the AGY test.
fn plugin_test_env() -> (tempfile::TempDir, PathBuf, EnvGuard) {
    let guard = EnvGuard::new();
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("HCOM_DIR", home.join(".hcom"));
        std::env::remove_var("CURSOR_CONFIG_DIR");
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        std::env::remove_var("GEMINI_CLI_HOME");
    }
    // Config's test guard redirects any unregistered HCOM_DIR to a
    // throwaway location (see config.rs from_env) — claude_config_dir
    // resolves through cached Config, so without this the settings file
    // this test writes and the one the verifier reads would disagree.
    crate::paths::test_roots::register(&home);
    crate::config::Config::reset();
    crate::config::Config::init();
    (dir, home, guard)
}

fn write_complete_skill_payload(root: &std::path::Path) {
    let skill = root.join("skills").join("hcom-agent-messaging");
    for relative in super::PLUGIN_SKILL_FILES {
        let path = skill.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("fixture for {relative}\n")).unwrap();
    }
}

fn write_stage_source(root: &std::path::Path) {
    write_complete_skill_payload(root);
    let adapter = root.join("plugin/hcom-agy");
    std::fs::create_dir_all(adapter.join(".claude-plugin")).unwrap();
    std::fs::create_dir_all(adapter.join("hooks")).unwrap();
    std::fs::write(
        adapter.join(".claude-plugin/plugin.json"),
        r#"{"name":"hcom","version":"1.0.0"}"#,
    )
    .unwrap();
    std::fs::write(adapter.join("hooks/hooks.json"), r#"{"hooks":{}}"#).unwrap();
}

fn relative_files(root: &std::path::Path) -> Vec<String> {
    fn visit(root: &std::path::Path, current: &std::path::Path, files: &mut Vec<String>) {
        for entry in std::fs::read_dir(current).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, files);
            } else {
                files.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace(std::path::MAIN_SEPARATOR, "/"),
                );
            }
        }
    }

    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort();
    files
}

#[test]
fn review_regression_production_staging_needs_no_external_shell_tools() {
    let source = tempfile::tempdir().unwrap();
    write_stage_source(source.path());

    let (_staging, artifact) = super::stage_plugin_artifact(source.path(), "hcom-agy")
        .expect("native staging must not require scripts/stage-plugin.sh or sh");

    assert!(artifact.join(".claude-plugin/plugin.json").is_file());
    assert!(artifact.join("hooks/hooks.json").is_file());
    assert!(super::verify_plugin_skill_payload(&artifact).is_ok());
}

#[test]
fn review_regression_agy_install_runner_receives_the_complete_staged_artifact() {
    let source = tempfile::tempdir().unwrap();
    write_stage_source(source.path());
    let inspected = std::cell::Cell::new(false);

    super::install_agy_plugin_from_root(
        source.path(),
        |artifact| {
            assert!(artifact.join(".claude-plugin/plugin.json").is_file());
            assert!(artifact.join("hooks/hooks.json").is_file());
            assert!(super::verify_plugin_skill_payload(artifact).is_ok());
            assert!(
                !artifact
                    .canonicalize()
                    .unwrap()
                    .starts_with(source.path().canonicalize().unwrap()),
                "install runner received a path inside the source checkout"
            );
            inspected.set(true);
            Ok(())
        },
        || true,
        || true,
    )
    .unwrap();

    assert!(
        inspected.get(),
        "install runner never inspected staged source"
    );
}

#[test]
fn review_regression_missing_checkout_falls_back_to_the_published_agy_repo() {
    let seen = std::cell::RefCell::new(String::new());

    super::install_agy_plugin_from_url(
        super::HCOM_PLUGIN_REPOSITORY_URL,
        |url| {
            *seen.borrow_mut() = url.to_string();
            Ok(())
        },
        || true,
        || true,
    )
    .unwrap();

    assert_eq!(seen.into_inner(), super::HCOM_PLUGIN_REPOSITORY_URL);
    assert!(
        super::HCOM_PLUGIN_REPOSITORY_URL.starts_with("https://"),
        "agy only recognizes https:// as a remote, not git@host:path: {}",
        super::HCOM_PLUGIN_REPOSITORY_URL
    );
}

#[cfg(unix)]
#[test]
fn review_regression_staging_rejects_skill_links_outside_the_source_tree() {
    use std::os::unix::fs::symlink;

    let source = tempfile::tempdir().unwrap();
    write_stage_source(source.path());
    let external = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(external.path(), "outside source tree\n").unwrap();
    let linked = source
        .path()
        .join("skills/hcom-agent-messaging/references/cross-tool.md");
    std::fs::remove_file(&linked).unwrap();
    symlink(external.path(), &linked).unwrap();

    let error = super::stage_plugin_artifact(source.path(), "hcom-agy").unwrap_err();

    assert!(
        error.contains("outside"),
        "outside-source link must be rejected explicitly: {error}"
    );
}

#[test]
fn plugin_skill_payload_rejects_hooks_only() {
    let fixture = tempfile::tempdir().unwrap();
    let hooks = fixture.path().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(hooks.join("hooks.json"), "{}").unwrap();

    let error = super::verify_plugin_skill_payload(fixture.path()).unwrap_err();
    assert!(error.contains("SKILL.md"), "unexpected error: {error}");
}

#[test]
fn plugin_skill_payload_rejects_skill_without_references() {
    let fixture = tempfile::tempdir().unwrap();
    let skill = fixture.path().join("skills/hcom-agent-messaging");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(skill.join("SKILL.md"), "fixture").unwrap();

    let error = super::verify_plugin_skill_payload(fixture.path()).unwrap_err();
    assert!(
        error.contains("references/cross-tool.md"),
        "unexpected error: {error}"
    );
}

#[cfg(unix)]
#[test]
fn plugin_skill_payload_rejects_external_skill_symlink() {
    use std::os::unix::fs::symlink;

    let fixture = tempfile::tempdir().unwrap();
    let artifact = fixture.path().join("artifact");
    let external = fixture.path().join("external");
    std::fs::create_dir_all(&artifact).unwrap();
    write_complete_skill_payload(&external);
    symlink(external.join("skills"), artifact.join("skills")).unwrap();

    let error = super::verify_plugin_skill_payload(&artifact).unwrap_err();
    assert!(
        error.contains("outside plugin artifact"),
        "unexpected error: {error}"
    );
}

#[test]
fn plugin_skill_payload_accepts_complete_artifact() {
    let fixture = tempfile::tempdir().unwrap();
    write_complete_skill_payload(fixture.path());

    assert!(super::verify_plugin_skill_payload(fixture.path()).is_ok());
}

#[test]
fn plugin_skill_payload_inventory_matches_every_canonical_file() {
    let canonical =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/hcom-agent-messaging");
    let actual = relative_files(&canonical);
    let mut verified = super::PLUGIN_SKILL_FILES
        .iter()
        .map(|relative| (*relative).to_string())
        .collect::<Vec<_>>();
    verified.sort();

    assert_eq!(
        verified, actual,
        "verify_plugin_skill_payload inventory must cover every canonical skill file"
    );
}

#[test]
#[serial]
fn agy_imported_hcom_source_reads_a_foreign_import() {
    let (_dir, home, _guard) = plugin_test_env();
    let config_dir = home.join(".gemini").join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("import_manifest.json"),
        r#"{"imports":[{"name":"hcom","source":"claude-code","importedAt":"2026-01-01T00:00:00Z","components":["hooks"]}]}"#,
    )
    .unwrap();
    assert_eq!(
        super::agy_imported_hcom_source().as_deref(),
        Some("claude-code")
    );
}

#[test]
#[serial]
fn agy_imported_hcom_source_is_none_without_a_manifest() {
    let (_dir, _home, _guard) = plugin_test_env();
    assert_eq!(super::agy_imported_hcom_source(), None);
}

/// Write an import entry plus an installed manifest, and read the state back.
fn agy_state_with(manifest: &str) -> super::AgyHooks {
    let config_dir = crate::runtime_env::gemini_family_config_dir().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    // `hcom hooks add antigravity` produces exactly this entry: labelled
    // claude-code, because our manifest dir is `.claude-plugin/`.
    std::fs::write(
        config_dir.join("import_manifest.json"),
        r#"{"imports":[{"name":"hcom","source":"claude-code","importedAt":"2026-01-01T00:00:00Z","components":["hooks"]}]}"#,
    )
    .unwrap();
    let hooks_path = super::agy_plugin_dir().join(super::AGY_HOOKS_RELATIVE);
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    std::fs::write(&hooks_path, manifest).unwrap();
    super::agy_hook_state()
}

/// A stand-in for one Claude-shaped entry.
const CLAUDE_ENTRY: &str =
    r#"{"name":"hcom-sessionstart","type":"command","command":"hcom claude-sessionstart"}"#;

#[test]
#[serial]
fn agy_hook_state_is_hcom_for_the_bundled_manifest() {
    let (_dir, _home, _guard) = plugin_test_env();
    // The real thing, not a hand-written stand-in: if the shipped manifest
    // ever stops satisfying this check, the check is what is wrong.
    let manifest = include_str!("../../plugin/hcom-agy/hooks/hooks.json");
    assert_eq!(agy_state_with(manifest), super::AgyHooks::Hcom);
}

#[test]
#[serial]
fn agy_hook_state_is_foreign_only_for_a_claude_shaped_manifest() {
    let (_dir, _home, _guard) = plugin_test_env();
    // SessionStart carrying a real command, and not one of our handlers
    // anywhere: that shape is the evidence, so naming the source is warranted.
    let manifest = format!(
        r#"{{"hooks":{{"SessionStart":[{CLAUDE_ENTRY}],"PostToolUse":[{CLAUDE_ENTRY}]}}}}"#
    );
    assert_eq!(
        agy_state_with(&manifest),
        super::AgyHooks::Foreign("claude-code".to_string())
    );
    // A bare key is not a handler.
    assert_eq!(
        agy_state_with(r#"{"hooks":{"SessionStart":[]}}"#),
        super::AgyHooks::Malformed
    );
}

#[test]
#[serial]
fn agy_hook_state_is_malformed_for_present_but_useless_events() {
    let (_dir, _home, _guard) = plugin_test_env();
    // Each of these parses, carries our event names, and cannot wake an agent.
    // A presence check would call every one of them healthy — and none of them
    // is evidence that another harness installed anything.
    for manifest in [
        r#"{"hooks":{"PreInvocation":[],"PostInvocation":[]}}"#,
        r#"{"hooks":{"PreInvocation":null,"PostInvocation":null}}"#,
        r#"{"hooks":{"PreInvocation":[null],"PostInvocation":[null]}}"#,
        r#"{"hooks":{"PreInvocation":[{"type":"command","command":"true"}],
                     "PostInvocation":[{"type":"command","command":"true"}]}}"#,
        // The word "hcom" without a handler that does anything.
        r#"{"hooks":{"PreInvocation":[{"type":"command","command":"hcom --version"}],
                     "PostInvocation":[{"type":"command","command":"hcom --version"}]}}"#,
        // Not a command entry — it runs nothing.
        r#"{"hooks":{"PreInvocation":[{"type":"http","command":"hcom gemini-beforeagent"}],
                     "PostInvocation":[{"type":"http","command":"hcom gemini-afteragent"}]}}"#,
        // Half an install: PostInvocation missing entirely.
        r#"{"hooks":{"PreInvocation":[{"type":"command","command":"hcom gemini-beforeagent"}]}}"#,
    ] {
        assert_eq!(
            agy_state_with(manifest),
            super::AgyHooks::Malformed,
            "must be reported as broken, not as another harness's: {manifest}"
        );
    }
}

#[test]
#[serial]
fn agy_hook_state_is_malformed_for_mutations_of_the_bundled_manifest() {
    let (_dir, _home, _guard) = plugin_test_env();
    let bundled = include_str!("../../plugin/hcom-agy/hooks/hooks.json");

    let swapped = bundled.replace("gemini-beforeagent", "claude-sessionstart");
    assert_eq!(agy_state_with(&swapped), super::AgyHooks::Malformed);

    let no_after = bundled.replace("gemini-afteragent", "gemini-sessionstart");
    assert_eq!(agy_state_with(&no_after), super::AgyHooks::Malformed);

    for renamed in [
        bundled.replace("gemini-beforeagent", "gemini-beforeagent-disabled"),
        bundled.replace("gemini-beforeagent", "x-gemini-beforeagent"),
        bundled.replace("gemini-afteragent", "gemini-afteragent2"),
    ] {
        assert_eq!(
            agy_state_with(&renamed),
            super::AgyHooks::Malformed,
            "a handler name that merely contains ours is not ours"
        );
    }

    let shared_only = r#"{"hooks":{"PostToolUse":[{"matcher":".*","hooks":[
        {"type":"command","command":"hcom gemini-aftertool"}]}]}}"#;
    assert_eq!(agy_state_with(shared_only), super::AgyHooks::Malformed);
}

#[test]
#[serial]
fn agy_hook_state_is_unverifiable_for_broken_json() {
    let (_dir, _home, _guard) = plugin_test_env();
    assert_eq!(agy_state_with("{not json"), super::AgyHooks::Unverifiable);
}

#[test]
fn install_does_not_strip_legacy_when_the_cli_fails() {
    let stripped = std::cell::Cell::new(false);
    let outcome = super::install_then_strip(
        || Err("marketplace add failed: network unreachable".to_string()),
        || false, // verify says not installed
        || {
            stripped.set(true);
            true
        },
    );
    assert!(outcome.is_err(), "failed install must report an error");
    assert!(
        !stripped.get(),
        "legacy hooks must survive a failed install"
    );
}

#[test]
fn install_does_not_strip_legacy_when_verify_fails() {
    let stripped = std::cell::Cell::new(false);
    let outcome = super::install_then_strip(
        || Ok(()), // CLI claims success
        || false,  // but verify disagrees
        || {
            stripped.set(true);
            true
        },
    );
    assert!(outcome.is_err());
    assert!(!stripped.get(), "verify is the gate, not the CLI exit code");
}

/// A strip that fails must not read as success: the user would be told the
/// migration completed while both hook sets stay live and double-fire.
#[test]
fn install_reports_a_failed_strip() {
    let outcome = super::install_then_strip(|| Ok(()), || true, || false);
    assert!(outcome.is_err(), "a failed strip must surface");
    assert!(
        outcome.unwrap_err().contains("legacy hook"),
        "the error must name what went wrong"
    );
}

/// Regression guard for a defect caught in review: gating Cursor's strip on
/// `verify_cursor_plugin_installed` risks deleting the user's hooks while
/// the plugin is disabled or the cache entry is stale, because that
/// verifier only proves the plugin materialized into Cursor's plugin
/// cache — not that the user enabled it in `/plugins`, and whether the
/// cache entry survives a disable is unmeasured. Cursor's enabled marker
/// is not readable from disk either, so no gate is honest — the source
/// must simply never strip.
#[test]
fn cursor_installer_never_strips_legacy_hooks() {
    let src = include_str!("plugin.rs");
    let body = src
        .split_once("pub(crate) fn install_cursor_plugin")
        .expect("install_cursor_plugin must exist")
        .1
        .split_once("\n}")
        .expect("function must be brace-terminated")
        .0;
    assert!(
        !body.contains("remove_cursor_hooks"),
        "install_cursor_plugin must not strip legacy hooks; found:\n{body}"
    );
}

#[test]
fn install_strips_legacy_only_after_verify_passes() {
    let stripped = std::cell::Cell::new(false);
    let outcome = super::install_then_strip(
        || Ok(()),
        || true,
        || {
            stripped.set(true);
            true
        },
    );
    assert!(outcome.is_ok());
    assert!(stripped.get());
}

/// The strip runs on this user's real machine, where settings.json also holds
/// agentpet, rtk, and herdr entries. Losing those would be a worse bug than
/// the one we are fixing.
#[test]
#[serial]
fn strip_preserves_hooks_owned_by_other_tools() {
    let (_dir, home, _guard) = plugin_test_env();
    let settings = home.join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        r#"{
          "hooks": {
            "SessionStart": [
              {"hooks":[{"type":"command","command":"bash '/home/u/.claude/hooks/herdr-agent-state.sh' session"}]},
              {"hooks":[{"type":"command","command":"/home/u/.local/bin/agentpet-hook claude"}]},
              {"hooks":[{"type":"command","command":"cmd=${HCOM:-hcom}; command -v \"${cmd%% *}\" >/dev/null 2>&1 && exec $cmd sessionstart || exit 0"}]}
            ],
            "PreToolUse": [
              {"hooks":[{"type":"command","command":"rtk hook claude"}]}
            ]
          }
        }"#,
    )
    .unwrap();

    crate::hooks::claude::remove_claude_hooks();

    let after = std::fs::read_to_string(&settings).unwrap();
    assert!(
        after.contains("herdr-agent-state.sh"),
        "herdr hook lost:\n{after}"
    );
    assert!(
        after.contains("agentpet-hook"),
        "agentpet hook lost:\n{after}"
    );
    assert!(after.contains("rtk hook claude"), "rtk hook lost:\n{after}");
    assert!(
        !after.contains("exec $cmd sessionstart"),
        "hcom hook survived:\n{after}"
    );
}

/// A strip that could not parse the file must not report success: hcom's
/// entries may still be in there, so "migration finished" would be a lie
/// and both hook sets would keep firing unnoticed.
#[test]
#[serial]
fn strip_reports_failure_on_malformed_json() {
    let (_dir, home, _guard) = plugin_test_env();
    let settings = home.join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, "{ this is not json").unwrap();

    assert!(
        !crate::hooks::claude::remove_claude_hooks(),
        "an unparseable settings.json must report a failed strip"
    );
}

#[test]
#[serial]
fn strip_leaves_malformed_json_untouched() {
    let (_dir, home, _guard) = plugin_test_env();
    let settings = home.join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let broken = "{ this is not json";
    std::fs::write(&settings, broken).unwrap();

    crate::hooks::claude::remove_claude_hooks();

    assert_eq!(
        std::fs::read_to_string(&settings).unwrap(),
        broken,
        "a file we cannot parse must not be rewritten"
    );
}

/// Builds a complete, valid Claude install under `home`. Returns the
/// installPath so tests can break each vertex individually.
fn write_healthy_claude_install(home: &std::path::Path) -> std::path::PathBuf {
    let plugins = home.join(".claude/plugins");
    let install_path = plugins.join("cache/hcom/hcom/1.0.1");
    std::fs::create_dir_all(&install_path).unwrap();

    std::fs::write(
        home.join(".claude/settings.json"),
        r#"{"enabledPlugins":{"hcom@hcom":true}}"#,
    )
    .unwrap();
    std::fs::write(
        plugins.join("known_marketplaces.json"),
        r#"{"hcom":{"source":{"source":"git","url":"https://github.com/sirassss/hcom-plugin"}}}"#,
    )
    .unwrap();
    std::fs::write(
        plugins.join("installed_plugins.json"),
        serde_json::json!({
            "plugins": {
                "hcom@hcom": [{
                    "scope": "user",
                    "installPath": install_path.to_string_lossy(),
                    "version": "1.0.1"
                }]
            }
        })
        .to_string(),
    )
    .unwrap();
    install_path
}

#[test]
#[serial]
fn claude_verify_needs_both_the_registry_and_the_enabled_flag() {
    let (_dir, home, _guard) = plugin_test_env();

    let settings = home.join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();

    // Neither half present.
    std::fs::write(&settings, r#"{}"#).unwrap();
    assert!(!super::verify_claude_plugin_installed());

    // Enabled flag only, no registry.
    std::fs::write(&settings, r#"{"enabledPlugins":{"hcom@hcom":true}}"#).unwrap();
    assert!(!super::verify_claude_plugin_installed());

    // Registry only, no enabled flag.
    write_healthy_claude_install(&home);
    std::fs::write(&settings, r#"{}"#).unwrap();
    assert!(!super::verify_claude_plugin_installed());

    // Both halves present.
    std::fs::write(&settings, r#"{"enabledPlugins":{"hcom@hcom":true}}"#).unwrap();
    assert!(super::verify_claude_plugin_installed());

    // Explicitly disabled by the user.
    std::fs::write(&settings, r#"{"enabledPlugins":{"hcom@hcom":false}}"#).unwrap();
    assert!(!super::verify_claude_plugin_installed());
}

#[test]
#[serial]
fn claude_verifier_accepts_a_healthy_install() {
    let (_dir, home, _guard) = plugin_test_env();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    write_healthy_claude_install(&home);
    assert!(super::verify_claude_plugin_installed());
}

#[test]
#[serial]
fn claude_verifier_rejects_a_removed_marketplace() {
    let (_dir, home, _guard) = plugin_test_env();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    write_healthy_claude_install(&home);

    // The user ran `claude plugin marketplace remove hcom`; the cache and
    // enabledPlugins are still present.
    std::fs::write(
        home.join(".claude/plugins/known_marketplaces.json"),
        r#"{"superpowers-marketplace":{}}"#,
    )
    .unwrap();

    assert!(
        !super::verify_claude_plugin_installed(),
        "an orphaned cache must not read as an installed plugin"
    );
}

#[test]
#[serial]
fn claude_verifier_rejects_a_dangling_install_path() {
    let (_dir, home, _guard) = plugin_test_env();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let install_path = write_healthy_claude_install(&home);
    std::fs::remove_dir_all(&install_path).unwrap();

    assert!(!super::verify_claude_plugin_installed());
}

/// Writes `import_manifest.json` with a single `hcom` entry carrying the
/// given `components`. Matches the real shape (module doc, plugin.rs:181
/// and the `agy_state_with` fixture above): `agy plugin install` records
/// the import regardless of whether the hook file it points at is
/// actually present, which is exactly why the verifier cannot trust this
/// file alone.
fn write_agy_import_manifest(components: &[&str]) {
    let manifest_path = super::agy_import_manifest();
    std::fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
    let components_json = serde_json::to_string(components).unwrap();
    std::fs::write(
        &manifest_path,
        format!(
            r#"{{"imports":[{{"name":"hcom","source":"claude-code","importedAt":"2026-01-01T00:00:00Z","components":{components_json}}}]}}"#
        ),
    )
    .unwrap();
}

fn write_agy_hook_file() {
    let hooks_path = super::agy_plugin_dir().join(super::AGY_HOOKS_RELATIVE);
    std::fs::create_dir_all(hooks_path.parent().unwrap()).unwrap();
    std::fs::write(&hooks_path, r#"{"hooks":{}}"#).unwrap();
}

#[test]
#[serial]
fn agy_verify_needs_the_hook_file_not_just_the_directory() {
    let (_dir, _home, _guard) = plugin_test_env();
    let plugin_dir = super::agy_plugin_dir();

    std::fs::create_dir_all(&plugin_dir).unwrap();
    assert!(
        !super::verify_agy_plugin_installed(),
        "empty dir is not installed"
    );

    write_agy_import_manifest(&["hooks"]);
    std::fs::create_dir_all(plugin_dir.join(super::AGY_HOOKS_RELATIVE).parent().unwrap()).unwrap();
    std::fs::write(
        plugin_dir.join(super::AGY_HOOKS_RELATIVE),
        r#"{"hooks":{}}"#,
    )
    .unwrap();
    assert!(super::verify_agy_plugin_installed());
}

/// Regression for the orphan-dir case the old file-only check missed: a
/// stale or hand-extracted copy of the plugin directory (no import ever
/// ran, or `agy plugin uninstall` cleared the manifest without deleting
/// the files) carries the hook file but has no manifest entry backing it.
#[test]
#[serial]
fn agy_verifier_rejects_orphan_dir_absent_from_manifest() {
    let (_dir, _home, _guard) = plugin_test_env();
    write_agy_hook_file();

    // No import_manifest.json at all.
    assert!(
        !super::verify_agy_plugin_installed(),
        "hook file with no manifest at all must not read as installed"
    );

    // Manifest exists but has no hcom entry.
    let manifest_path = super::agy_import_manifest();
    std::fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
    std::fs::write(&manifest_path, r#"{"imports":[]}"#).unwrap();
    assert!(
        !super::verify_agy_plugin_installed(),
        "hook file with no hcom entry in the manifest must not read as installed"
    );

    // hcom entry present, but its components don't include "hooks" (e.g.
    // only skills were imported).
    write_agy_import_manifest(&["skills"]);
    assert!(
        !super::verify_agy_plugin_installed(),
        "an hcom entry without a hooks component must not read as installed"
    );
}

#[test]
#[serial]
fn agy_verifier_accepts_manifest_entry_with_hooks_component() {
    let (_dir, _home, _guard) = plugin_test_env();
    write_agy_hook_file();
    write_agy_import_manifest(&["hooks"]);
    assert!(super::verify_agy_plugin_installed());

    // Real installs also carry "skills" alongside "hooks" — order/extra
    // entries must not matter, only that "hooks" is present.
    write_agy_import_manifest(&["skills", "hooks"]);
    assert!(super::verify_agy_plugin_installed());
}

/// Writes a completed Cursor-owned cache entry, the shape measured on a
/// real host: `<cache>/<marketplace>/<plugin>/<sha>/` carrying
/// `.cache-complete` and `hooks/hooks-cursor.json`.
fn write_cursor_cache_entry(home: &std::path::Path) -> std::path::PathBuf {
    let cache = home
        .join(".cursor/plugins/cache")
        .join(super::CLAUDE_MARKETPLACE)
        .join(super::PLUGIN_NAME)
        .join("a1511e68");
    let hooks = cache.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(hooks.join("hooks-cursor.json"), "{}").unwrap();
    write_complete_skill_payload(&cache);
    std::fs::write(cache.join(".cache-complete"), "").unwrap();
    cache
}

/// M6: a real cache entry can carry `.cache-complete` and
/// `hooks/hooks-cursor.json` while `skills` is a dangling symlink — the
/// old verifier checked neither, so it reported this entry installed
/// while the messaging skill was actually unreadable.
#[cfg(unix)]
#[test]
#[serial]
fn cursor_verifier_rejects_cache_with_dangling_skills_symlink() {
    use std::os::unix::fs::symlink;

    let (_dir, home, _guard) = plugin_test_env();
    let cache = home
        .join(".cursor/plugins/cache")
        .join(super::CLAUDE_MARKETPLACE)
        .join(super::PLUGIN_NAME)
        .join("a1511e68");
    let hooks = cache.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(hooks.join("hooks-cursor.json"), "{}").unwrap();
    std::fs::write(cache.join(".cache-complete"), "").unwrap();
    // Never points anywhere real, same shape as the measured M6 host.
    symlink(cache.join("../../skills"), cache.join("skills")).unwrap();

    assert!(
        !super::verify_cursor_plugin_installed(),
        "a cache entry whose skill payload is a dangling symlink must not verify as installed"
    );
    assert!(
        super::cursor_cache_entry_missing_skill_payload().is_some(),
        "Task 11: a cache entry with both marker files but a broken skill payload must \
         be distinguishable from no cache entry at all"
    );
}

/// Task 11: a healthy cache entry, and no cache entry at all, must not be
/// reported as "skill payload missing" — that message is for the one
/// state in between.
#[test]
#[serial]
fn cursor_cache_entry_missing_skill_payload_is_none_outside_the_broken_state() {
    let (_dir, home, _guard) = plugin_test_env();
    assert!(
        super::cursor_cache_entry_missing_skill_payload().is_none(),
        "no cache dir at all must not report a broken payload"
    );

    write_cursor_cache_entry(&home);
    assert!(
        super::cursor_cache_entry_missing_skill_payload().is_none(),
        "a fully healthy cache entry must not report a broken payload"
    );
}

/// Task 10a — the measured truth table the Cursor/Claude coupling rests on.
///
/// Measured on a real host 2026-09-19: a Cursor agent spawned with **zero**
/// Cursor-side artifacts (registry, cache and all three stale checkouts
/// removed) still reported `bindings: hooks, pty` and loaded the messaging
/// skill out of `~/.claude/plugins/cache/hcom/hcom/1.0.1/`. Installing both
/// produced no duplicate: the skill appeared once and the delivery/start
/// event counts matched the Claude-only case exactly.
///
/// So the row that matters is (claude=true, cursor=false): Cursor hooks are
/// live there, yet `verify_cursor_plugin_installed` reports false, because
/// it only ever looks at Cursor's own cache. This test pins that gap as a
/// measured fact rather than an assumption, and is the ground truth the
/// planned `cursor_hooks_covered()` has to satisfy — it must be true in
/// every row below except (false, false).
///
/// Cursor's marketplace registry is account state with no backing file, so
/// no file-only verifier can consult it. Borrowing Claude's state is not a
/// shortcut here, it is the only signal available on the pre-spawn path,
/// which is barred from spawning a subprocess.
#[test]
#[serial]
fn cursor_and_claude_verifier_truth_table() {
    for (claude, cursor) in [(false, false), (false, true), (true, false), (true, true)] {
        let (_dir, home, _guard) = plugin_test_env();

        if claude {
            write_healthy_claude_install(&home);
            let settings = home.join(".claude/settings.json");
            std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
            std::fs::write(&settings, r#"{"enabledPlugins":{"hcom@hcom":true}}"#).unwrap();
        }
        if cursor {
            write_cursor_cache_entry(&home);
        }

        assert_eq!(
            super::verify_claude_plugin_installed(),
            claude,
            "claude verifier, row ({claude}, {cursor})"
        );
        assert_eq!(
            super::verify_cursor_plugin_installed(),
            cursor,
            "cursor verifier reads only Cursor's own cache, row ({claude}, {cursor})"
        );

        // What Task 1 will add. Kept as a local expression so the table
        // records the intended semantics before the function exists.
        let covered =
            super::verify_claude_plugin_installed() || super::verify_cursor_plugin_installed();
        assert_eq!(
            covered,
            claude || cursor,
            "hooks-covered, row ({claude}, {cursor})"
        );
    }
}

#[test]
#[serial]
fn cursor_is_covered_when_claude_plugin_installed() {
    let (_dir, home, _guard) = plugin_test_env();

    write_healthy_claude_install(&home);
    let settings = home.join(".claude/settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, r#"{"enabledPlugins":{"hcom@hcom":true}}"#).unwrap();

    assert!(
        !super::verify_cursor_plugin_installed(),
        "no Cursor-side artifacts exist in this fixture"
    );
    assert!(super::cursor_hooks_covered());
}

#[test]
#[serial]
fn cursor_not_covered_when_neither_installed() {
    let (_dir, _home, _guard) = plugin_test_env();

    assert!(!super::verify_claude_plugin_installed());
    assert!(!super::verify_cursor_plugin_installed());
    assert!(!super::cursor_hooks_covered());
}

#[test]
#[serial]
fn cursor_covered_by_its_own_cache_without_claude() {
    let (_dir, home, _guard) = plugin_test_env();

    write_cursor_cache_entry(&home);

    assert!(!super::verify_claude_plugin_installed());
    assert!(super::cursor_hooks_covered());
}

#[test]
#[serial]
fn cursor_verify_requires_a_completed_cache_entry() {
    let (_dir, _home, _guard) = plugin_test_env();

    // No cache directory at all.
    assert!(!super::verify_cursor_plugin_installed());

    let cache = _home
        .join(".cursor/plugins/cache")
        .join(super::CLAUDE_MARKETPLACE)
        .join(super::PLUGIN_NAME)
        .join("a1511e68");

    // A nested-but-wrong file: present somewhere under the cache entry,
    // but not at the exact path the verifier requires, and no
    // `.cache-complete` either. Proves the walk checks the specific
    // files, not "any file exists under a cache entry".
    let hooks = cache.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(hooks.join("not-the-hook-file.json"), "{}").unwrap();
    assert!(!super::verify_cursor_plugin_installed());

    // The real hook file lands but the entry still isn't marked
    // complete.
    std::fs::write(hooks.join("hooks-cursor.json"), "{}").unwrap();
    assert!(!super::verify_cursor_plugin_installed());

    // `.cache-complete` alone still isn't enough without the skill payload.
    std::fs::write(cache.join(".cache-complete"), "").unwrap();
    assert!(!super::verify_cursor_plugin_installed());

    // Once the skill payload is complete too, the entry counts.
    write_complete_skill_payload(&cache);
    assert!(super::verify_cursor_plugin_installed());
}

#[test]
#[serial]
fn cursor_verifier_reads_the_plugin_cache_not_a_marketplace_checkout() {
    let (_dir, home, _guard) = plugin_test_env();

    // Checkout of the OLD marketplace repo, in the exact layout the old
    // verifier accepted.
    let stale = home
        .join(".cursor/plugins/marketplaces/github.com/sirassss/hcom/60dc686")
        .join("plugin/hcom/hooks");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("hooks-cursor.json"), "{}").unwrap();

    assert!(
        !super::verify_cursor_plugin_installed(),
        "a stale marketplace checkout must not count as an installed plugin"
    );
}

const CLAUDE_MANIFEST: &str = include_str!("../../plugin/hcom/hooks/hooks.json");

#[test]
fn claude_manifest_covers_every_configured_event() {
    let root: Value = serde_json::from_str(CLAUDE_MANIFEST).unwrap();
    let hooks = root["hooks"].as_object().expect("hooks object");

    for (event, matcher, suffix, timeout) in crate::hooks::claude::CLAUDE_HOOK_CONFIGS {
        let entries = hooks
            .get(*event)
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("missing event {event}"));

        // Compare against what production writes into settings.json rather
        // than a substring: byte equality also catches a mangled
        // `command -v` guard, and needs no reasoning about suffixes that
        // prefix each other (`post` vs `post-failure`).
        let expected_command = crate::hooks::claude::build_hook_entry_command(suffix);
        let group = entries
            .iter()
            .find(|g| {
                g["hooks"].as_array().is_some_and(|inner| {
                    inner
                        .iter()
                        .any(|h| h["command"].as_str() == Some(expected_command.as_str()))
                })
            })
            .unwrap_or_else(|| panic!("no hcom command for {event} -> {suffix}"));

        if matcher.is_empty() {
            assert!(
                group.get("matcher").is_none(),
                "{event} should have no matcher"
            );
        } else {
            assert_eq!(group["matcher"], *matcher, "{event} matcher");
        }

        // A dropped timeout is silent breakage: the legacy verifier treats
        // it as fatal (VerifyFailReason::HookTimeoutMissing), and Stop /
        // PostToolUse / SubagentStop rely on the long value to poll.
        assert_eq!(
            group["hooks"][0].get("timeout").and_then(Value::as_u64),
            *timeout,
            "{event} timeout"
        );
    }

    // Table -> JSON above; JSON -> table here, so a stray event cannot ride
    // along unnoticed.
    assert_eq!(
        hooks.len(),
        crate::hooks::claude::CLAUDE_HOOK_CONFIGS.len(),
        "manifest has events the table does not: {:?}",
        hooks.keys().collect::<Vec<_>>()
    );
}

#[test]
fn claude_manifest_commands_fail_open() {
    let root: Value = serde_json::from_str(CLAUDE_MANIFEST).unwrap();
    for (_event, entries) in root["hooks"].as_object().unwrap() {
        for group in entries.as_array().unwrap() {
            for hook in group["hooks"].as_array().unwrap() {
                let cmd = hook["command"].as_str().unwrap();
                assert!(
                    cmd.contains("command -v") && cmd.contains("|| exit 0"),
                    "command must exit 0 when hcom is absent: {cmd}"
                );
                assert_eq!(hook["type"], "command");
            }
        }
    }
}

const CURSOR_MANIFEST: &str = include_str!("../../plugin/hcom/hooks/hooks-cursor.json");
const CURSOR_DESCRIPTOR: &str = include_str!("../../plugin/hcom/.cursor-plugin/plugin.json");

#[test]
fn cursor_manifest_covers_every_configured_event() {
    let root: Value = serde_json::from_str(CURSOR_MANIFEST).unwrap();
    assert_eq!(root["version"], 1, "Cursor requires a top-level version");
    let hooks = root["hooks"].as_object().expect("hooks object");

    for (event, suffix) in crate::hooks::cursor::CURSOR_HOOK_COMMANDS {
        let entries = hooks
            .get(*event)
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("missing event {event}"));

        // Cursor's manifest must not use build_cursor_hook_command: that
        // function embeds get_hcom_prefix(), resolved at install time, and
        // a committed manifest is a static file. Task 2 solved the same
        // problem for Claude with a self-resolving builder; reuse it here
        // rather than duplicating it under a Cursor-specific name.
        // Find the hcom entry first, then assert every field on that one
        // entry. Scanning the array separately per field would let a
        // foreign entry supply a correct timeout while ours carries a
        // wrong one.
        let expected_command = crate::hooks::claude::build_hook_entry_command(suffix);
        let entry = entries
            .iter()
            .find(|h| h["command"].as_str() == Some(expected_command.as_str()))
            .unwrap_or_else(|| panic!("no hcom command for {event} -> {suffix}"));

        let expected_timeout = if *event == "stop" {
            crate::hooks::cursor::STOP_HOOK_TIMEOUT_SECS
        } else {
            crate::hooks::cursor::HOOK_TIMEOUT_SECS
        };
        assert_eq!(
            entry["timeout"].as_u64(),
            Some(expected_timeout),
            "{event} timeout"
        );

        // Cursor caps a stop hook's follow-up loop unless the entry opts
        // out with an explicit null, so the field must be present, not
        // merely absent-and-defaulted. `entry["loop_limit"].is_null()`
        // would not say that: serde_json's Index yields Null for a missing
        // key, so it passes either way. `get` distinguishes them — the
        // same idiom `verify_hooks_at` uses in src/hooks/cursor.rs.
        //
        // Note this manifest is not verifiable by `verify_hooks_at`: that
        // function compares commands against `build_cursor_hook_command`
        // ("hcom cursor-stop"), while these carry the self-resolving guard.
        // The plugin path verifies by file presence instead.
        if *event == "stop" {
            assert!(
                entry.get("loop_limit").is_some_and(Value::is_null),
                "stop must carry an explicit loop_limit: null"
            );
        }
    }

    assert_eq!(
        hooks.len(),
        crate::hooks::cursor::CURSOR_HOOK_COMMANDS.len(),
        "manifest has events the table does not: {:?}",
        hooks.keys().collect::<Vec<_>>()
    );
}

#[test]
fn cursor_descriptor_points_at_its_own_hook_file() {
    let d: Value = serde_json::from_str(CURSOR_DESCRIPTOR).unwrap();
    assert_eq!(d["name"], super::PLUGIN_NAME);
    assert_eq!(d["hooks"], "./hooks/hooks-cursor.json");
    // Cursor, unlike Claude, finds nothing by convention — an undeclared
    // component is simply absent. Dropping this key would install hcom into
    // Cursor without the skill that teaches an agent to use it.
    assert_eq!(d["skills"], "./skills/");
}

/// `plugin/hcom/skills` is a committed directory, so the declared path
/// resolves. It used to be a symlink to the repo-root `skills/`; Codex's
/// installer skipped that link and shipped a package with hooks and no
/// skill, so every adapter now carries real files generated by
/// `scripts/sync-plugin-skills.sh` (see `tests/plugin_payload.rs`).
#[test]
fn cursor_declared_skills_path_resolves() {
    let skills = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("plugin")
        .join("hcom")
        .join("skills");
    assert!(skills.is_dir(), "{} is not a directory", skills.display());
    assert!(
        skills.join("hcom-agent-messaging").is_dir(),
        "hcom-agent-messaging missing under {}",
        skills.display()
    );
}

const CODEX_MANIFEST: &str = include_str!("../../plugin/hcom/hooks/hooks-codex.json");
const CODEX_DESCRIPTOR: &str = include_str!("../../plugin/hcom/.codex-plugin/plugin.json");

/// Drives off `CODEX_HOOK_COMMANDS` — the same table the native installer
/// builds its hook JSON from — so an event, subcommand or matcher change on
/// the writer side fails here instead of silently diverging from the
/// committed overlay. Commands use the self-resolving guard, not
/// `build_codex_hook_command`: that one embeds `get_hcom_prefix()` resolved
/// at install time, and a committed manifest is a static file (same reason
/// spelled out for Cursor above). Native Codex hook JSON carries no
/// timeouts, so the overlay carries none either.
#[test]
fn codex_manifest_covers_every_configured_event() {
    let root: Value = serde_json::from_str(CODEX_MANIFEST).unwrap();
    let hooks = root["hooks"].as_object().expect("hooks object");

    for (event, suffix, matcher) in crate::hooks::codex::CODEX_HOOK_COMMANDS {
        let groups = hooks
            .get(*event)
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("missing event {event}"));
        let expected_command = crate::hooks::claude::build_hook_entry_command(suffix);
        let group = groups
            .iter()
            .find(|g| {
                g["hooks"]
                    .as_array()
                    .is_some_and(|inner| inner.iter().any(|h| h["command"] == expected_command))
            })
            .unwrap_or_else(|| panic!("no hcom command for {event} -> {suffix}"));

        assert_eq!(
            group.get("matcher").and_then(Value::as_str),
            *matcher,
            "{event} matcher"
        );
        let hook = group["hooks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|h| h["command"] == expected_command)
            .unwrap();
        assert_eq!(hook["type"], "command", "{event} hook type");
        assert!(
            hook.get("timeout").is_none(),
            "{event} must match the native payload, which sets no timeout"
        );
    }

    // Anything beyond the table would ship a handler Codex never fires, or
    // a Claude-only event (SessionEnd, PostToolUseFailure) that the native
    // integration deliberately omits.
    assert_eq!(
        hooks.len(),
        crate::hooks::codex::CODEX_HOOK_COMMANDS.len(),
        "manifest has events the table does not: {:?}",
        hooks.keys().collect::<Vec<_>>()
    );
}

/// A command parked on the wrong event must fail the contract above; this
/// pins that the check is event-scoped, not a whole-file substring scan.
#[test]
fn codex_manifest_check_rejects_a_command_on_the_wrong_event() {
    let mut root: Value = serde_json::from_str(CODEX_MANIFEST).unwrap();
    let stop = root["hooks"]["Stop"].take();
    root["hooks"]["UserPromptSubmit"] = stop;

    let expected = crate::hooks::claude::build_hook_entry_command("codex-userpromptsubmit");
    assert!(
        !root["hooks"]["UserPromptSubmit"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["hooks"]
                .as_array()
                .is_some_and(|inner| inner.iter().any(|h| h["command"] == expected))),
        "swapped event still satisfied its own command"
    );
}

/// The selector both Claude and Codex install by must name the marketplace
/// that `.claude-plugin/marketplace.json` actually declares, and the plugin
/// inside it. A rename there silently breaks `codex plugin add`.
#[test]
fn the_plugin_selector_matches_the_committed_marketplace() {
    let marketplace: Value =
        serde_json::from_str(include_str!("../../.claude-plugin/marketplace.json")).unwrap();
    let (plugin, market) = super::CLAUDE_PLUGIN_ID.split_once('@').unwrap();
    assert_eq!(marketplace["name"], market);
    assert_eq!(super::CLAUDE_MARKETPLACE, market);
    let entries = marketplace["plugins"].as_array().unwrap();
    let entry = entries
        .iter()
        .find(|p| p["name"] == plugin)
        .unwrap_or_else(|| panic!("marketplace declares no plugin named {plugin}"));
    // Codex, Claude and Cursor all install the same package directory.
    assert_eq!(entry["source"], "./plugin/hcom");
}

/// Codex, like Cursor, resolves nothing by convention: an undeclared
/// component is simply absent.
#[test]
fn codex_descriptor_points_at_its_own_hook_file() {
    let d: Value = serde_json::from_str(CODEX_DESCRIPTOR).unwrap();
    assert_eq!(d["name"], super::PLUGIN_NAME);
    assert_eq!(d["hooks"], "./hooks/hooks-codex.json");
    assert_eq!(d["skills"], "./skills/");
}

/// One package, one version: Claude reads `.claude-plugin/`, Cursor
/// `.cursor-plugin/` and Codex `.codex-plugin/` out of the same directory,
/// so a drifting version would report three different releases of one
/// install.
#[test]
fn shared_package_descriptors_agree_on_name_and_version() {
    let claude: Value =
        serde_json::from_str(include_str!("../../plugin/hcom/.claude-plugin/plugin.json")).unwrap();
    for other in [CURSOR_DESCRIPTOR, CODEX_DESCRIPTOR] {
        let d: Value = serde_json::from_str(other).unwrap();
        assert_eq!(d["name"], claude["name"]);
        assert_eq!(d["version"], claude["version"]);
    }
}

const AGY_MANIFEST: &str = include_str!("../../plugin/hcom-agy/hooks/hooks.json");
const AGY_DESCRIPTOR: &str = include_str!("../../plugin/hcom-agy/.claude-plugin/plugin.json");

/// Find the entry named `name` inside an event's array, regardless of
/// whether the event nests under `hooks: [...]` (PreToolUse/PostToolUse,
/// which also carry a matcher on the outer group) or carries `name`/
/// `command` directly on the array element (the three lifecycle events,
/// where PreInvocation holds two such elements side by side). Returns the
/// outer group (for matcher) alongside the resolved hook object (for
/// everything else) — for a flat entry these are the same value.
fn agy_group_and_hook<'a>(root: &'a Value, event: &str, name: &str) -> (&'a Value, &'a Value) {
    let entries = root["hooks"][event]
        .as_array()
        .unwrap_or_else(|| panic!("missing event {event}"));
    for group in entries {
        if let Some(inner) = group["hooks"].as_array() {
            if let Some(hook) = inner.iter().find(|h| h["name"] == name) {
                return (group, hook);
            }
        } else if group["name"] == name {
            return (group, group);
        }
    }
    panic!("no entry named {name} under event {event}");
}

/// Drives off `AGY_HOOK_CONFIGS` — the same table `try_setup_antigravity_hooks`
/// builds its `json!` from — instead of a hand-copied local table, so a
/// writer-side change to a timeout, subcommand, matcher, description or
/// fallback fails this test instead of silently diverging from the
/// committed manifest.
#[test]
fn agy_manifest_matches_live_installer_exactly() {
    let root: Value = serde_json::from_str(AGY_MANIFEST).unwrap();

    for &(event, name, suffix, matcher, fallback, description) in
        crate::hooks::antigravity::AGY_HOOK_CONFIGS
    {
        let on_missing = if fallback.is_empty() {
            "exit 0".to_string()
        } else {
            use base64::Engine;
            let b64 = base64::engine::general_purpose::STANDARD.encode(fallback.as_bytes());
            format!("{{ printf %s {b64} | base64 -d; exit 0; }}")
        };
        let guard = crate::hooks::claude::build_hook_entry_command_with(
            suffix,
            "ANTIGRAVITY_AGENT=1 ",
            &on_missing,
        );
        let expected_command = format!("sh -c '{guard}'");

        let (group, hook) = agy_group_and_hook(&root, event, name);

        // Built from the shared builder, not reconstructed independently:
        // a changed subcommand or fallback in AGY_HOOK_CONFIGS changes
        // expected_command too, so it can't drift from the manifest
        // without this assertion catching it.
        assert_eq!(
            hook["command"].as_str(),
            Some(expected_command.as_str()),
            "{event}/{name} command"
        );
        assert_eq!(hook["name"].as_str(), Some(name), "{event}/{name} name");
        assert_eq!(
            hook["type"].as_str(),
            Some("command"),
            "{event}/{name} type"
        );
        assert_eq!(
            hook.get("timeout").and_then(Value::as_u64),
            Some(crate::hooks::antigravity::HOOK_TIMEOUT_SEC),
            "{event}/{name} timeout"
        );
        assert_eq!(
            hook["description"].as_str(),
            Some(description),
            "{event}/{name} description"
        );

        if matcher.is_empty() {
            assert!(
                group.get("matcher").is_none(),
                "{event}/{name} should have no matcher"
            );
        } else {
            assert_eq!(
                group.get("matcher").and_then(Value::as_str),
                Some(matcher),
                "{event}/{name} matcher"
            );
        }
    }

    // Manifest's event set must equal the table's — a stray top-level
    // event (e.g. an extra "SessionStart": []) contributes zero entries
    // to the total-count check below, so it needs its own assertion.
    let hooks = root["hooks"].as_object().unwrap();
    let manifest_events: std::collections::HashSet<&str> =
        hooks.keys().map(String::as_str).collect();
    let table_events: std::collections::HashSet<&str> = crate::hooks::antigravity::AGY_HOOK_CONFIGS
        .iter()
        .map(|row| row.0)
        .collect();
    assert_eq!(manifest_events, table_events, "manifest events vs table");

    // Every entry the table declares — and no more. Counts both the outer
    // arrays (lifecycle events) and the inner `hooks` arrays nested under
    // a matcher (PreToolUse/PostToolUse), so an entry smuggled into either
    // shape is caught, not just a stray top-level event.
    let total_entries: usize = hooks
        .values()
        .map(|entries| {
            entries
                .as_array()
                .unwrap()
                .iter()
                .map(|group| match group.get("hooks").and_then(Value::as_array) {
                    Some(inner) => inner.len(),
                    None => 1,
                })
                .sum::<usize>()
        })
        .sum();
    assert_eq!(
        total_entries,
        crate::hooks::antigravity::AGY_HOOK_CONFIGS.len(),
        "manifest has hook entries the table does not account for"
    );
}

/// Whole-file sweep, mirroring `claude_manifest_commands_fail_open`: every
/// command in the AGY manifest — not just the six the table goes looking
/// for — must carry the ANTIGRAVITY_AGENT=1 marker that routes it away
/// from Claude's handler.
#[test]
fn agy_manifest_commands_carry_antigravity_marker() {
    let root: Value = serde_json::from_str(AGY_MANIFEST).unwrap();
    for (event, entries) in root["hooks"].as_object().unwrap() {
        for group in entries.as_array().unwrap() {
            let hooks: Vec<&Value> = match group.get("hooks").and_then(Value::as_array) {
                Some(inner) => inner.iter().collect(),
                None => vec![group],
            };
            for hook in hooks {
                let cmd = hook["command"].as_str().unwrap();
                assert!(
                    cmd.contains("ANTIGRAVITY_AGENT=1"),
                    "{event}: command missing ANTIGRAVITY_AGENT=1 marker: {cmd}"
                );
                assert_eq!(hook["type"], "command");
            }
        }
    }
}

#[test]
fn agy_descriptor_declares_the_plugin_name() {
    let d: Value = serde_json::from_str(AGY_DESCRIPTOR).unwrap();
    assert_eq!(d["name"], super::PLUGIN_NAME);
    assert!(d["version"].is_string());
}

/// The whole reason for a second plugin directory: Antigravity and Claude
/// read the same conventional path, so the two files at that path must not
/// be the same file's content.
#[test]
fn agy_and_claude_conventional_manifests_are_disjoint() {
    assert_ne!(
        AGY_MANIFEST, CLAUDE_MANIFEST,
        "hooks/hooks.json must differ between plugin/hcom and plugin/hcom-agy"
    );
    let claude_events: std::collections::HashSet<String> =
        serde_json::from_str::<Value>(CLAUDE_MANIFEST).unwrap()["hooks"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
    let agy_events: std::collections::HashSet<String> = serde_json::from_str::<Value>(AGY_MANIFEST)
        .unwrap()["hooks"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert!(
        agy_events.contains("PreInvocation"),
        "AGY manifest lost its own event vocabulary: {agy_events:?}"
    );
    assert!(
        !claude_events.contains("PreInvocation"),
        "Claude manifest must not carry AGY events: {claude_events:?}"
    );

    // The name-check above only ever probed one event in each direction.
    // PreToolUse/PostToolUse/Stop are genuinely shared vocabulary — AGY
    // borrows Claude's event names by design — so a literal empty-
    // intersection assertion is not achievable here (verified: the real
    // intersection is {PostToolUse, PreToolUse, Stop}). What must not
    // happen is a Claude-only event leaking into AGY's file (or vice
    // versa): compare the file-level overlap against the overlap the two
    // production tables themselves declare, so any *extra* shared name
    // — one the tables don't already share — fails.
    let agy_table_events: std::collections::HashSet<&str> =
        crate::hooks::antigravity::AGY_HOOK_CONFIGS
            .iter()
            .map(|row| row.0)
            .collect();
    let claude_table_events: std::collections::HashSet<&str> =
        crate::hooks::claude::CLAUDE_HOOK_CONFIGS
            .iter()
            .map(|row| row.0)
            .collect();
    let shared_in_files: std::collections::HashSet<&str> = agy_events
        .intersection(&claude_events)
        .map(String::as_str)
        .collect();
    let shared_in_tables: std::collections::HashSet<&str> = agy_table_events
        .intersection(&claude_table_events)
        .copied()
        .collect();
    assert_eq!(
        shared_in_files, shared_in_tables,
        "manifests share event names neither production table shares"
    );
}

/// Neither manifest may invoke the other tool's subcommands. Driven off the
/// real tables rather than a hand-picked few: the earlier three-name list
/// let `notify`, `pre`, `subagent-stop` and seven others through, and
/// cross-fire between these two tools is the entire reason this plugin
/// exists. The `exec $cmd ` prefix anchors each needle, so `pre` does not
/// match `cursor-pretooluse`.
#[test]
fn manifests_never_call_the_other_tools_subcommands() {
    let cursor_text = CURSOR_MANIFEST.to_string();
    for (_event, _matcher, suffix, _timeout) in crate::hooks::claude::CLAUDE_HOOK_CONFIGS {
        assert!(
            !cursor_text.contains(&format!("exec $cmd {suffix} ")),
            "Cursor manifest calls Claude subcommand {suffix}"
        );
    }

    let claude_text = CLAUDE_MANIFEST.to_string();
    for (_event, suffix) in crate::hooks::cursor::CURSOR_HOOK_COMMANDS {
        assert!(
            !claude_text.contains(&format!("exec $cmd {suffix} ")),
            "Claude manifest calls Cursor subcommand {suffix}"
        );
    }

    // AGY is the sharpest case: it reads the same conventional
    // `hooks/hooks.json` path Claude does, so this is the exact
    // cross-fire the split directory exists to prevent.
    let agy_text = AGY_MANIFEST.to_string();
    for (_event, _matcher, suffix, _timeout) in crate::hooks::claude::CLAUDE_HOOK_CONFIGS {
        assert!(
            !agy_text.contains(&format!("exec $cmd {suffix} ")),
            "AGY manifest calls Claude subcommand {suffix}"
        );
    }
    for (_event, suffix) in crate::hooks::cursor::CURSOR_HOOK_COMMANDS {
        assert!(
            !agy_text.contains(&format!("exec $cmd {suffix} ")),
            "AGY manifest calls Cursor subcommand {suffix}"
        );
    }
}

/// Mọi manifest ta ship phải trỏ về repo thật sự chứa chúng. Trước đây cả
/// bốn cái đều ghi upstream, nên không có trường nào trên đĩa phân biệt
/// được một bản cài từ fork với một bản cài từ upstream.
#[test]
fn shipped_plugin_manifests_point_at_our_own_repo() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifests = [
        "plugin/hcom/.cursor-plugin/plugin.json",
        "plugin/hcom/.claude-plugin/plugin.json",
        "plugin/hcom/.codex-plugin/plugin.json",
        "plugin/hcom-agy/.claude-plugin/plugin.json",
    ];

    for relative in manifests {
        let path = repo_root.join(relative);
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

        for field in ["homepage", "repository"] {
            assert_eq!(
                json[field].as_str().unwrap(),
                super::HCOM_PLUGIN_REPOSITORY_URL,
                "{relative} field `{field}` must name the repo that ships it"
            );
        }
        // Ghi công tác giả gốc không được xoá cùng lúc.
        assert_eq!(json["author"]["name"].as_str().unwrap(), "aannoo");
        assert_eq!(json["license"].as_str().unwrap(), "MIT");
    }
}

/// Marketplace descriptor và plugin được publish cùng một lần bởi
/// `scripts/sync-plugin-skills.sh --publish`, nên version của chúng phải khớp.
#[test]
fn marketplace_and_plugin_versions_agree() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |relative: &str| -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(repo_root.join(relative)).unwrap()).unwrap()
    };

    assert_eq!(
        read("plugin/.claude-plugin/marketplace.json")["version"]
            .as_str()
            .unwrap(),
        read("plugin/hcom/.claude-plugin/plugin.json")["version"]
            .as_str()
            .unwrap(),
    );
}

// ── Uninstall command shapes ──────────────────────────────────────
//
// Pure argument-list assertions — no subprocess. The end-to-end effect
// (does `claude plugin marketplace remove hcom` actually clear the
// registry) is not verifiable without a real Claude/Cursor/Antigravity
// CLI and account state, so it stays a manual check.

#[test]
fn claude_uninstall_runs_uninstall_then_marketplace_remove() {
    let commands = super::claude_uninstall_commands();
    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0],
        (
            "claude",
            vec!["plugin", "uninstall", super::CLAUDE_PLUGIN_ID]
        )
    );
    assert_eq!(
        commands[1],
        (
            "claude",
            vec!["plugin", "marketplace", "remove", super::CLAUDE_MARKETPLACE]
        )
    );
}

#[test]
fn cursor_uninstall_removes_the_marketplace() {
    assert_eq!(
        super::cursor_uninstall_command(),
        (
            "cursor-agent",
            vec!["plugin", "marketplace", "remove", super::PLUGIN_NAME]
        )
    );
}

#[test]
fn agy_uninstall_uninstalls_the_plugin() {
    assert_eq!(
        super::agy_uninstall_command(),
        ("agy", vec!["plugin", "uninstall", super::PLUGIN_NAME])
    );
}

/// Uninstall must not shell out at all when the tool reports no plugin —
/// otherwise every `hcom hooks remove <tool>` on a machine that never
/// installed the plugin would spawn a CLI call that fails with a noisy
/// "not installed" error. Verified by pointing verify at an empty test
/// env rather than by mocking `run_tool_cli`, matching the pattern the
/// verify tests above already use.
#[test]
#[serial]
fn uninstall_is_a_noop_when_nothing_is_installed() {
    let (_dir, _home, _guard) = plugin_test_env();
    assert!(!super::verify_claude_plugin_installed());
    assert!(!super::claude_plugin_has_any_trace());
    assert!(super::uninstall_claude_plugin().is_ok());
    assert!(!super::verify_cursor_plugin_installed());
    assert!(!super::cursor_uninstall_should_attempt());
    assert!(super::uninstall_cursor_plugin().is_ok());
    assert!(!super::verify_agy_plugin_installed());
    assert!(super::uninstall_agy_plugin().is_ok());
}

/// Regression for the gap this task closes: `hcom hooks add cursor`
/// registers the marketplace and returns `Err` telling the user to finish
/// in `/plugins` — no cache entry exists yet at that point. Gating
/// removal on the strict, cache-only verifier made `hcom hooks remove
/// cursor` a silent no-op in exactly this state, leaving the marketplace
/// registered forever. The registry list (mocked here via
/// `HCOM_TEST_CURSOR_MARKETPLACE_LIST`) is what now proves it, not an
/// on-disk checkout.
#[test]
#[serial]
fn cursor_uninstall_attempts_removal_when_marketplace_registered_but_plugin_never_installed() {
    let (_dir, _home, _guard) = plugin_test_env();
    let _list = EnvVarGuard::set(
        "HCOM_TEST_CURSOR_MARKETPLACE_LIST",
        "hcom  https://github.com/sirassss/hcom-plugin\n",
    );

    assert!(
        !super::verify_cursor_plugin_installed(),
        "no plugin cache exists yet in this state"
    );
    assert!(
        super::cursor_uninstall_should_attempt(),
        "a registered-but-never-installed marketplace must not read as nothing to clean up"
    );
}

/// Fixture matches the REAL `cursor-agent plugin marketplace list` shape
/// measured 2026-09-19 (cursor-agent 2026.09.18-9a7762b): a whitespace
/// table with several other marketplaces, one of them (`cursor-public`)
/// a built-in `global` entry with no URL column at all. hcom's own row
/// still carries its URL, so the URL-substring match alone would pass
/// here too — this test locks in that the name-column match also fires,
/// so the check keeps working if Cursor ever drops the URL column.
#[test]
#[serial]
fn cursor_registry_lists_hcom_matches_real_marketplace_list_shape() {
    let (_dir, _home, _guard) = plugin_test_env();
    let _list = EnvVarGuard::set(
        "HCOM_TEST_CURSOR_MARKETPLACE_LIST",
        "cursor-public     global  \n\
         hcom              user    https://github.com/sirassss/hcom-plugin\n\
         i-have-adhd       user    https://github.com/ayghri/i-have-adhd\n\
         ponytail          user    https://github.com/DietrichGebert/ponytail\n",
    );
    assert!(super::cursor_registry_lists_hcom());
}

/// Name-only match must still hold if the URL column is ever dropped —
/// simulates that by listing hcom with no URL at all (as `cursor-public`
/// actually appears in the real output above).
#[test]
#[serial]
fn cursor_registry_lists_hcom_matches_on_name_alone() {
    let (_dir, _home, _guard) = plugin_test_env();
    let _list = EnvVarGuard::set(
        "HCOM_TEST_CURSOR_MARKETPLACE_LIST",
        "hcom              user  \n",
    );
    assert!(super::cursor_registry_lists_hcom());
}

/// Task 4's red test: `cursor_marketplace_checkout_exists`, the function
/// this replaces, matched a leftover on-disk marketplace checkout that a
/// successful `cursor-agent plugin marketplace remove` never cleans up
/// (plan M3) — so it stayed stuck at `true` forever once a checkout had
/// ever existed, making `hcom hooks remove cursor` fail with "No
/// marketplace matches" in an infinite loop (plan M4). The registry
/// listing is the actual source of truth: a stale directory must not
/// override what it says.
#[test]
#[serial]
fn cursor_uninstall_does_not_attempt_when_registry_lacks_hcom() {
    let (_dir, home, _guard) = plugin_test_env();
    // Simulates the exact M4 state: a checkout directory survives a
    // completed removal.
    let checkout =
        home.join(".cursor/plugins/marketplaces/github.com/sirassss/hcom-plugin/abc1234");
    std::fs::create_dir_all(&checkout).unwrap();
    let _list = EnvVarGuard::set(
        "HCOM_TEST_CURSOR_MARKETPLACE_LIST",
        "some-other-marketplace  https://github.com/someone/else\n",
    );

    assert!(!super::verify_cursor_plugin_installed());
    assert!(
        !super::cursor_uninstall_should_attempt(),
        "a stale on-disk checkout must not override a registry listing that lacks hcom"
    );
}

/// Same M4 shape, the artifact this test's sibling above didn't cover:
/// a fully MATERIALIZED cache (not just a bare checkout dir) survives
/// `marketplace remove` untouched (measured 2026-09-20, real host) —
/// `verify_cursor_plugin_installed()` reads `true` from that alone. An
/// earlier version of `cursor_uninstall_should_attempt` OR'd that verifier
/// in, so this exact state made `hcom hooks remove cursor` retry the
/// removal CLI and print "No marketplace matches" forever, on every
/// single invocation — the loop `cursor_registry_lists_hcom` was built to
/// close, reopened through the cache instead of the checkout.
#[test]
#[serial]
fn cursor_uninstall_does_not_attempt_when_cache_survives_but_registry_lacks_hcom() {
    let (_dir, home, _guard) = plugin_test_env();
    let cache_root = home.join(".cursor/plugins/cache/hcom/hcom/deadbeef");
    std::fs::create_dir_all(cache_root.join("hooks")).unwrap();
    std::fs::write(cache_root.join(".cache-complete"), "").unwrap();
    std::fs::write(cache_root.join("hooks").join("hooks-cursor.json"), "{}").unwrap();
    let skill_root = cache_root.join("skills").join("hcom-agent-messaging");
    for relative in super::PLUGIN_SKILL_FILES {
        let path = skill_root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "content").unwrap();
    }
    let _list = EnvVarGuard::set(
        "HCOM_TEST_CURSOR_MARKETPLACE_LIST",
        "some-other-marketplace  https://github.com/someone/else\n",
    );

    assert!(
        super::verify_cursor_plugin_installed(),
        "a fully materialized cache must read as installed"
    );
    assert!(
        !super::cursor_uninstall_should_attempt(),
        "a surviving cache must not override a registry listing that lacks hcom"
    );
}

/// Same gap, Claude side: the marketplace registration and cache can
/// exist while `enabledPlugins` never got a `hcom@hcom` entry (or was
/// stripped by hand), which fails the strict verifier's AND but is still
/// a marketplace registration `hooks remove claude` must clean up.
#[test]
#[serial]
fn claude_uninstall_attempts_removal_when_marketplace_registered_but_plugin_never_enabled() {
    let (_dir, home, _guard) = plugin_test_env();
    std::fs::create_dir_all(home.join(".claude/plugins")).unwrap();
    std::fs::write(
        home.join(".claude/plugins/known_marketplaces.json"),
        r#"{"hcom":{"source":{"source":"git","url":"https://github.com/sirassss/hcom-plugin"}}}"#,
    )
    .unwrap();

    assert!(!super::verify_claude_plugin_installed());
    assert!(
        super::claude_plugin_has_any_trace(),
        "a registered-but-never-enabled marketplace must not read as nothing to clean up"
    );
}

/// The other two vertices of `claude_plugin_has_any_trace`'s OR, isolated:
/// the function's own doc motivates it with "removed the marketplace by
/// hand while enabledPlugins and the install-path cache survive" — this
/// covers the `enabledPlugins`-only half of that exact state.
#[test]
#[serial]
fn claude_uninstall_attempts_removal_when_only_the_enabled_flag_remains() {
    let (_dir, home, _guard) = plugin_test_env();
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    // The marketplace and cache are already gone; only a leftover
    // enabledPlugins entry remains (even disabled, its presence alone is
    // a trace worth telling `claude plugin uninstall` about).
    std::fs::write(
        home.join(".claude/settings.json"),
        r#"{"enabledPlugins":{"hcom@hcom":false}}"#,
    )
    .unwrap();

    assert!(!super::verify_claude_plugin_installed());
    assert!(
        super::claude_plugin_has_any_trace(),
        "an enabledPlugins entry alone must not read as nothing to clean up"
    );
}

/// The install-path cache half of the same state: no enabledPlugins
/// entry, no known marketplace, only a leftover `installed_plugins.json`
/// record.
#[test]
#[serial]
fn claude_uninstall_attempts_removal_when_only_the_installed_plugins_registry_remains() {
    let (_dir, home, _guard) = plugin_test_env();
    let plugins = home.join(".claude/plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    std::fs::write(
        plugins.join("installed_plugins.json"),
        r#"{"plugins":{"hcom@hcom":[{"scope":"user","installPath":"/nonexistent","version":"1.0.1"}]}}"#,
    )
    .unwrap();

    assert!(!super::verify_claude_plugin_installed());
    assert!(
        super::claude_plugin_has_any_trace(),
        "an installed_plugins.json entry alone must not read as nothing to clean up"
    );
}
