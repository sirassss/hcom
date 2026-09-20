use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum VerifyFailReason {
    #[error("hooks.json settings unreadable or empty")]
    SettingsUnreadableOrEmpty,
    #[error("agy permissions missing or incomplete in: {0}")]
    PermissionsMissing(PathBuf),
    #[error("hooks.json missing 'hcom-lifecycle' group key")]
    HcomLifecycleKeyMissing,
    #[error("hook event '{0}' missing or empty")]
    HookEventMissing(String),
    #[error("hcom hook command '{cmd_suffix}' not found under event '{event}'")]
    HookCommandMissing { event: String, cmd_suffix: String },
    #[error("event '{0}': hcom entry has 'type' != \"command\"")]
    HookTypeFieldNotCommand(String),
    #[error("event '{event}' name mismatch: expected {expected:?}, got {actual:?}")]
    HookNameMismatch {
        event: String,
        expected: String,
        actual: String,
    },
    #[error("event '{event}' matcher mismatch: expected {expected:?}, got {actual:?}")]
    HookMatcherMismatch {
        event: String,
        expected: String,
        actual: String,
    },
    #[error("event '{event}' has no numeric 'timeout' field (canonical)")]
    HookTimeoutMissing { event: String },
    #[error("duplicate hcom hook entry for event '{0}'")]
    HookDuplicated(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("existing hooks.json at {} could not be read: {source}", path.display())]
    ExistingReadFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("existing hooks.json at {} is not valid JSON: {source}", path.display())]
    ExistingParseFailed {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("existing hooks.json at {} must be a JSON object", path.display())]
    ExistingRootNotObject { path: PathBuf },
    #[error("JSON serialization failed: {0}")]
    SerializationFailed(#[from] serde_json::Error),
    #[error("atomic write to {} failed: {source}", path.display())]
    AtomicWriteFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("post-write verify failed for {}: {reason}", path.display())]
    PostWriteVerifyFailed {
        path: PathBuf,
        #[source]
        reason: VerifyFailReason,
    },
}

/// Resolve the path to the Antigravity `hooks.json` file.
/// Under the split-config design, this resides at `~/.gemini/config/hooks.json`.
pub fn get_antigravity_hooks_path() -> PathBuf {
    antigravity_hooks_path(&crate::runtime_env::gemini_family_config_dir())
}

fn antigravity_hooks_path(gemini_dir: &Path) -> PathBuf {
    gemini_dir.join("config").join("hooks.json")
}

/// Shell wrapper for a single hcom hook subcommand (`gemini-beforeagent`, etc.).
///
/// `fallback_json` is echoed to stdout when hcom is missing, before exiting 0.
/// agy requires a `decision` JSON response on PreToolUse and Stop; PostToolUse and
/// PostInvocation accept an empty body.
///
/// The fallback is delivered base64-encoded and piped through `base64 -d` so the
/// JSON's quotes (and any apostrophes) survive the nested `sh -c '...'` pass —
/// naive interpolation gets stripped or mis-tokenized by the inner shell.
///
fn hook_sh_cmd(hcom_cmd: &str, subcmd: &str, fallback_json: &str) -> String {
    let bin = hcom_cmd.split_whitespace().next().unwrap_or("hcom");
    if cfg!(windows) {
        let invoke = format!("set \"ANTIGRAVITY_AGENT=1\" && {hcom_cmd} {subcmd}");
        if fallback_json.is_empty() {
            return format!("where {bin} >nul 2>nul && ({invoke}) || exit /b 0");
        }
        return format!(
            "where {bin} >nul 2>nul && ({invoke}) || (echo {fallback_json} & exit /b 0)"
        );
    }
    if fallback_json.is_empty() {
        format!(
            "sh -c 'command -v {bin} >/dev/null 2>&1 && ANTIGRAVITY_AGENT=1 exec {hcom_cmd} {subcmd} || exit 0'"
        )
    } else {
        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(fallback_json.as_bytes());
        format!(
            "sh -c 'command -v {bin} >/dev/null 2>&1 && ANTIGRAVITY_AGENT=1 exec {hcom_cmd} {subcmd} || {{ printf %s {b64} | base64 -d; exit 0; }}'"
        )
    }
}

/// PreInvocation sessionstart: invoke hcom; idempotent via `name_announced`
/// (bootstrap injection no-ops after first run, so re-firing per turn is safe).
fn hook_sessionstart_cmd(hcom_cmd: &str) -> String {
    hook_sh_cmd(hcom_cmd, "gemini-sessionstart", "")
}

/// Fallback JSON constant for hooks where agy requires a decision response when
/// hcom is missing. PreToolUse needs `{"decision":"allow"}`; Stop needs a decision
/// field where any value other than "continue" allows the stop. PostToolUse and the
/// *Invocation lifecycle hooks accept an empty body (`""` in [`AGY_HOOK_CONFIGS`]).
const ALLOW_JSON: &str = "{\"decision\":\"allow\"}";

/// 15s timeout: agy default is 30s; 5s was tight under cold-start + busy sqlite
/// on slower machines / CI. 15s leaves margin without leaving a stuck hook
/// blocking the agent turn for half a minute.
pub(crate) const HOOK_TIMEOUT_SEC: u64 = 15;

/// (event, entry name, subcommand, matcher, fallback JSON, description).
///
/// `""` means "no matcher" / "no fallback", the same convention
/// [`crate::hooks::claude::CLAUDE_HOOK_CONFIGS`] uses for an empty matcher.
///
/// The two events with a matcher (`PreToolUse`, `PostToolUse`) get nested
/// under `"hooks": [...]` by [`try_setup_antigravity_hooks`]; the lifecycle
/// events stay flat arrays. `PreInvocation` carries two rows, both landing in
/// the same array.
pub(crate) const AGY_HOOK_CONFIGS: &[(&str, &str, &str, &str, &str, &str)] = &[
    (
        "PreInvocation",
        "hcom-sessionstart",
        "gemini-sessionstart",
        "",
        "",
        "Initialize hcom session",
    ),
    (
        "PreInvocation",
        "hcom-beforeagent",
        "gemini-beforeagent",
        "",
        "",
        "Deliver pending messages",
    ),
    (
        "PostInvocation",
        "hcom-afteragent",
        "gemini-afteragent",
        "",
        "",
        "Signal ready for messages",
    ),
    (
        "Stop",
        "hcom-sessionend",
        "gemini-sessionend",
        "",
        ALLOW_JSON,
        "Disconnect from hcom",
    ),
    (
        "PreToolUse",
        "hcom-beforetool",
        "gemini-beforetool",
        ".*",
        ALLOW_JSON,
        "Track tool execution",
    ),
    (
        "PostToolUse",
        "hcom-aftertool",
        "gemini-aftertool",
        ".*",
        "",
        "Deliver messages after tools",
    ),
];

/// Build the `command` string for one [`AGY_HOOK_CONFIGS`] row. `hcom-sessionstart`
/// routes through [`hook_sessionstart_cmd`] (which takes no fallback); every other
/// row goes through [`hook_sh_cmd`] directly.
pub(crate) fn agy_hook_command(
    hcom_cmd: &str,
    name: &str,
    subcmd: &str,
    fallback_json: &str,
) -> String {
    if name == "hcom-sessionstart" {
        hook_sessionstart_cmd(hcom_cmd)
    } else {
        hook_sh_cmd(hcom_cmd, subcmd, fallback_json)
    }
}

/// Try to set up Antigravity hooks in `hooks.json`.
/// Reads existing hooks.json, merges "hcom-lifecycle" group, and preserves all other keys.
pub fn try_setup_antigravity_hooks(include_permissions: bool) -> Result<(), SetupError> {
    let hooks_path = get_antigravity_hooks_path();
    if let Some(parent) = hooks_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Load existing hooks or initialize an empty map
    let mut hooks_root = if hooks_path.exists() {
        let content = std::fs::read_to_string(&hooks_path).map_err(|source| {
            SetupError::ExistingReadFailed {
                path: hooks_path.clone(),
                source,
            }
        })?;
        let value: Value =
            serde_json::from_str(&content).map_err(|source| SetupError::ExistingParseFailed {
                path: hooks_path.clone(),
                source,
            })?;
        value
            .as_object()
            .cloned()
            .ok_or_else(|| SetupError::ExistingRootNotObject {
                path: hooks_path.clone(),
            })?
    } else {
        serde_json::Map::new()
    };

    let hcom_cmd = crate::runtime_env::build_hcom_command();

    // Preserve table row order within each event's array.
    let mut lifecycle_map = serde_json::Map::new();
    for &(event, name, subcmd, matcher, fallback, description) in AGY_HOOK_CONFIGS {
        let hook = json!({
            "name": name,
            "type": "command",
            "command": agy_hook_command(&hcom_cmd, name, subcmd, fallback),
            "timeout": HOOK_TIMEOUT_SEC,
            "description": description
        });
        let entry = if matcher.is_empty() {
            hook
        } else {
            json!({ "matcher": matcher, "hooks": [hook] })
        };
        lifecycle_map
            .entry(event.to_string())
            .or_insert_with(|| Value::Array(Vec::new()))
            .as_array_mut()
            .expect("lifecycle entries are arrays")
            .push(entry);
    }
    let hcom_lifecycle = Value::Object(lifecycle_map);

    hooks_root.insert("hcom-lifecycle".to_string(), hcom_lifecycle);

    let json_str = serde_json::to_string_pretty(&Value::Object(hooks_root))
        .map_err(SetupError::SerializationFailed)?;

    crate::paths::atomic_write_io(&hooks_path, &json_str).map_err(|e| {
        SetupError::AtomicWriteFailed {
            path: hooks_path.clone(),
            source: e,
        }
    })?;

    // Agy stores permissions in its own settings.json under `permissions.allow`
    // using `command(...)` rules (not the gemini-cli TOML policy engine).
    if include_permissions {
        setup_antigravity_permissions();
    } else {
        remove_antigravity_permissions();
    }

    verify_hooks_at(&hooks_path, include_permissions).map_err(|reason| {
        SetupError::PostWriteVerifyFailed {
            path: hooks_path,
            reason,
        }
    })?;

    Ok(())
}

/// Verify if Antigravity hooks are correctly installed.
pub fn verify_antigravity_hooks_installed(check_permissions: bool) -> bool {
    verify_hooks_at(&get_antigravity_hooks_path(), check_permissions).is_ok()
}

/// Cleanly remove the `"hcom-lifecycle"` group key from `hooks.json` and
/// strip hcom permission rules from `~/.gemini/antigravity-cli/settings.json`.
/// Preserves other hooks.json keys, and removes the file if no other keys remain.
///
/// Returns true only when BOTH the hooks cleanup and the permission cleanup
/// succeed. Permission cleanup is attempted unconditionally \u2014 even when
/// hooks.json is missing, unreadable, or invalid \u2014 so a partially broken
/// install does not leave stale `command(hcom ...)` allow-rules behind.
pub fn remove_antigravity_hooks() -> bool {
    antigravity_cleanup_dirs().iter().all(|dir| {
        remove_hooks_lifecycle_block_at(&antigravity_hooks_path(dir))
            && remove_antigravity_permissions_at(&antigravity_settings_path(dir))
    })
}

/// Strip just the `"hcom-lifecycle"` block from hooks.json. Returns true on
/// success, including the "file absent" case. Does not touch permissions.
fn remove_hooks_lifecycle_block_at(path: &Path) -> bool {
    if !path.exists() {
        return true;
    }
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut val: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let obj = match val.as_object_mut() {
        Some(o) => o,
        None => return false,
    };
    obj.remove("hcom-lifecycle");
    if obj.is_empty() {
        return std::fs::remove_file(path).is_ok();
    }
    let json_str = match serde_json::to_string_pretty(&Value::Object(obj.clone())) {
        Ok(s) => s,
        Err(_) => return false,
    };
    crate::paths::atomic_write_io(path, &json_str).is_ok()
}

fn verify_hooks_at(path: &Path, check_permissions: bool) -> Result<(), VerifyFailReason> {
    if !path.exists() {
        return Err(VerifyFailReason::SettingsUnreadableOrEmpty);
    }
    let content =
        std::fs::read_to_string(path).map_err(|_| VerifyFailReason::SettingsUnreadableOrEmpty)?;
    let val: Value =
        serde_json::from_str(&content).map_err(|_| VerifyFailReason::SettingsUnreadableOrEmpty)?;
    let root = val
        .as_object()
        .ok_or(VerifyFailReason::SettingsUnreadableOrEmpty)?;

    let lifecycle = root
        .get("hcom-lifecycle")
        .and_then(|v| v.as_object())
        .ok_or(VerifyFailReason::HcomLifecycleKeyMissing)?;

    // Check PreInvocation
    let pre_invocation = lifecycle
        .get("PreInvocation")
        .and_then(|v| v.as_array())
        .ok_or_else(|| VerifyFailReason::HookEventMissing("PreInvocation".to_string()))?;

    let mut found_sessionstart = false;
    let mut found_beforeagent = false;
    for hook in pre_invocation {
        let hook_obj = hook.as_object().ok_or_else(|| {
            VerifyFailReason::HookTypeFieldNotCommand("PreInvocation".to_string())
        })?;
        let name = hook_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let command = hook_obj
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let hook_type = hook_obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

        if hook_type != "command" {
            return Err(VerifyFailReason::HookTypeFieldNotCommand(
                "PreInvocation".to_string(),
            ));
        }
        if hook_obj.get("timeout").and_then(|v| v.as_u64()).is_none() {
            return Err(VerifyFailReason::HookTimeoutMissing {
                event: "PreInvocation".to_string(),
            });
        }

        if name == "hcom-sessionstart" {
            if found_sessionstart {
                return Err(VerifyFailReason::HookDuplicated(
                    "PreInvocation".to_string(),
                ));
            }
            if !command.contains("gemini-sessionstart") {
                return Err(VerifyFailReason::HookCommandMissing {
                    event: "PreInvocation".to_string(),
                    cmd_suffix: "gemini-sessionstart".to_string(),
                });
            }
            found_sessionstart = true;
        } else if name == "hcom-beforeagent" {
            if found_beforeagent {
                return Err(VerifyFailReason::HookDuplicated(
                    "PreInvocation".to_string(),
                ));
            }
            if !command.contains("gemini-beforeagent") {
                return Err(VerifyFailReason::HookCommandMissing {
                    event: "PreInvocation".to_string(),
                    cmd_suffix: "gemini-beforeagent".to_string(),
                });
            }
            found_beforeagent = true;
        }
    }
    if !found_sessionstart {
        return Err(VerifyFailReason::HookCommandMissing {
            event: "PreInvocation".to_string(),
            cmd_suffix: "gemini-sessionstart".to_string(),
        });
    }
    if !found_beforeagent {
        return Err(VerifyFailReason::HookCommandMissing {
            event: "PreInvocation".to_string(),
            cmd_suffix: "gemini-beforeagent".to_string(),
        });
    }

    // Check PostInvocation
    let post_invocation = lifecycle
        .get("PostInvocation")
        .and_then(|v| v.as_array())
        .ok_or_else(|| VerifyFailReason::HookEventMissing("PostInvocation".to_string()))?;
    let mut found_afteragent = false;
    for hook in post_invocation {
        let hook_obj = hook.as_object().ok_or_else(|| {
            VerifyFailReason::HookTypeFieldNotCommand("PostInvocation".to_string())
        })?;
        let name = hook_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let command = hook_obj
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let hook_type = hook_obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

        if hook_type != "command" {
            return Err(VerifyFailReason::HookTypeFieldNotCommand(
                "PostInvocation".to_string(),
            ));
        }
        if hook_obj.get("timeout").and_then(|v| v.as_u64()).is_none() {
            return Err(VerifyFailReason::HookTimeoutMissing {
                event: "PostInvocation".to_string(),
            });
        }

        if name == "hcom-afteragent" {
            if found_afteragent {
                return Err(VerifyFailReason::HookDuplicated(
                    "PostInvocation".to_string(),
                ));
            }
            if !command.contains("gemini-afteragent") {
                return Err(VerifyFailReason::HookCommandMissing {
                    event: "PostInvocation".to_string(),
                    cmd_suffix: "gemini-afteragent".to_string(),
                });
            }
            found_afteragent = true;
        }
    }
    if !found_afteragent {
        return Err(VerifyFailReason::HookCommandMissing {
            event: "PostInvocation".to_string(),
            cmd_suffix: "gemini-afteragent".to_string(),
        });
    }

    // Check Stop
    let stop = lifecycle
        .get("Stop")
        .and_then(|v| v.as_array())
        .ok_or_else(|| VerifyFailReason::HookEventMissing("Stop".to_string()))?;
    let mut found_sessionend = false;
    for hook in stop {
        let hook_obj = hook
            .as_object()
            .ok_or_else(|| VerifyFailReason::HookTypeFieldNotCommand("Stop".to_string()))?;
        let name = hook_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let command = hook_obj
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let hook_type = hook_obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

        if hook_type != "command" {
            return Err(VerifyFailReason::HookTypeFieldNotCommand(
                "Stop".to_string(),
            ));
        }
        if hook_obj.get("timeout").and_then(|v| v.as_u64()).is_none() {
            return Err(VerifyFailReason::HookTimeoutMissing {
                event: "Stop".to_string(),
            });
        }

        if name == "hcom-sessionend" {
            if found_sessionend {
                return Err(VerifyFailReason::HookDuplicated("Stop".to_string()));
            }
            if !command.contains("gemini-sessionend") {
                return Err(VerifyFailReason::HookCommandMissing {
                    event: "Stop".to_string(),
                    cmd_suffix: "gemini-sessionend".to_string(),
                });
            }
            found_sessionend = true;
        }
    }
    if !found_sessionend {
        return Err(VerifyFailReason::HookCommandMissing {
            event: "Stop".to_string(),
            cmd_suffix: "gemini-sessionend".to_string(),
        });
    }

    // Check PreToolUse
    let pre_tool_use = lifecycle
        .get("PreToolUse")
        .and_then(|v| v.as_array())
        .ok_or_else(|| VerifyFailReason::HookEventMissing("PreToolUse".to_string()))?;
    let mut found_beforetool = false;
    for matcher_val in pre_tool_use {
        let matcher_obj = matcher_val
            .as_object()
            .ok_or_else(|| VerifyFailReason::HookTypeFieldNotCommand("PreToolUse".to_string()))?;
        let matcher_pattern = matcher_obj
            .get("matcher")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if matcher_pattern != ".*" {
            return Err(VerifyFailReason::HookMatcherMismatch {
                event: "PreToolUse".to_string(),
                expected: ".*".to_string(),
                actual: matcher_pattern.to_string(),
            });
        }
        let hooks_arr = matcher_obj
            .get("hooks")
            .and_then(|v| v.as_array())
            .ok_or_else(|| VerifyFailReason::HookTypeFieldNotCommand("PreToolUse".to_string()))?;
        for hook in hooks_arr {
            let hook_obj = hook.as_object().ok_or_else(|| {
                VerifyFailReason::HookTypeFieldNotCommand("PreToolUse".to_string())
            })?;
            let name = hook_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let command = hook_obj
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let hook_type = hook_obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

            if hook_type != "command" {
                return Err(VerifyFailReason::HookTypeFieldNotCommand(
                    "PreToolUse".to_string(),
                ));
            }
            if hook_obj.get("timeout").and_then(|v| v.as_u64()).is_none() {
                return Err(VerifyFailReason::HookTimeoutMissing {
                    event: "PreToolUse".to_string(),
                });
            }

            if name == "hcom-beforetool" {
                if found_beforetool {
                    return Err(VerifyFailReason::HookDuplicated("PreToolUse".to_string()));
                }
                if !command.contains("gemini-beforetool") {
                    return Err(VerifyFailReason::HookCommandMissing {
                        event: "PreToolUse".to_string(),
                        cmd_suffix: "gemini-beforetool".to_string(),
                    });
                }
                found_beforetool = true;
            }
        }
    }
    if !found_beforetool {
        return Err(VerifyFailReason::HookCommandMissing {
            event: "PreToolUse".to_string(),
            cmd_suffix: "gemini-beforetool".to_string(),
        });
    }

    // Check PostToolUse
    let post_tool_use = lifecycle
        .get("PostToolUse")
        .and_then(|v| v.as_array())
        .ok_or_else(|| VerifyFailReason::HookEventMissing("PostToolUse".to_string()))?;
    let mut found_aftertool = false;
    for matcher_val in post_tool_use {
        let matcher_obj = matcher_val
            .as_object()
            .ok_or_else(|| VerifyFailReason::HookTypeFieldNotCommand("PostToolUse".to_string()))?;
        let matcher_pattern = matcher_obj
            .get("matcher")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if matcher_pattern != ".*" {
            return Err(VerifyFailReason::HookMatcherMismatch {
                event: "PostToolUse".to_string(),
                expected: ".*".to_string(),
                actual: matcher_pattern.to_string(),
            });
        }
        let hooks_arr = matcher_obj
            .get("hooks")
            .and_then(|v| v.as_array())
            .ok_or_else(|| VerifyFailReason::HookTypeFieldNotCommand("PostToolUse".to_string()))?;
        for hook in hooks_arr {
            let hook_obj = hook.as_object().ok_or_else(|| {
                VerifyFailReason::HookTypeFieldNotCommand("PostToolUse".to_string())
            })?;
            let name = hook_obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let command = hook_obj
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let hook_type = hook_obj.get("type").and_then(|v| v.as_str()).unwrap_or("");

            if hook_type != "command" {
                return Err(VerifyFailReason::HookTypeFieldNotCommand(
                    "PostToolUse".to_string(),
                ));
            }
            if hook_obj.get("timeout").and_then(|v| v.as_u64()).is_none() {
                return Err(VerifyFailReason::HookTimeoutMissing {
                    event: "PostToolUse".to_string(),
                });
            }

            if name == "hcom-aftertool" {
                if found_aftertool {
                    return Err(VerifyFailReason::HookDuplicated("PostToolUse".to_string()));
                }
                if !command.contains("gemini-aftertool") {
                    return Err(VerifyFailReason::HookCommandMissing {
                        event: "PostToolUse".to_string(),
                        cmd_suffix: "gemini-aftertool".to_string(),
                    });
                }
                found_aftertool = true;
            }
        }
    }
    if !found_aftertool {
        return Err(VerifyFailReason::HookCommandMissing {
            event: "PostToolUse".to_string(),
            cmd_suffix: "gemini-aftertool".to_string(),
        });
    }

    // Check permissions in agy settings.json
    if check_permissions {
        let settings_path = get_antigravity_settings_path();
        if !antigravity_permissions_complete(&settings_path) {
            return Err(VerifyFailReason::PermissionsMissing(settings_path));
        }
    }

    Ok(())
}

/// Path to the Antigravity CLI's settings.json (under `~/.gemini/antigravity-cli/`).
fn get_antigravity_settings_path() -> PathBuf {
    antigravity_settings_path(&crate::runtime_env::gemini_family_config_dir())
}

fn antigravity_settings_path(gemini_dir: &Path) -> PathBuf {
    gemini_dir.join("antigravity-cli").join("settings.json")
}

fn antigravity_cleanup_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut push_unique = |dir: PathBuf| {
        if dir.is_absolute() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };
    if let Some(home) = dirs::home_dir() {
        push_unique(home.join(".gemini"));
    }
    if let Ok(dir) = std::env::var("GEMINI_CLI_HOME")
        && !dir.is_empty()
    {
        // GEMINI_CLI_HOME is a raw prefix; gemini_family_config_dir() appends
        // .gemini, so mirror that here.
        let p = PathBuf::from(dir);
        push_unique(p.join(".gemini"));
    }
    push_unique(crate::runtime_env::gemini_family_config_dir());
    dirs
}

/// Build the list of `command(...)` rules for safe hcom commands.
fn antigravity_permission_rules() -> Vec<String> {
    let mut rules = Vec::new();
    for prefix in &["hcom", "uvx hcom"] {
        for cmd in crate::hooks::common::SAFE_HCOM_COMMANDS {
            rules.push(format!("command({} {})", prefix, cmd));
        }
    }
    rules
}

/// Merge hcom permission rules into `~/.gemini/antigravity-cli/settings.json`.
/// Preserves any other keys and pre-existing entries in `permissions.allow`.
fn setup_antigravity_permissions() -> bool {
    let path = get_antigravity_settings_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut root = if path.exists() {
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str::<Value>(&s)
                .ok()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default(),
            Err(_) => return false,
        }
    } else {
        serde_json::Map::new()
    };

    let permissions = root
        .entry("permissions".to_string())
        .or_insert_with(|| json!({}));
    let permissions_obj = match permissions.as_object_mut() {
        Some(o) => o,
        None => return false,
    };
    let allow = permissions_obj
        .entry("allow".to_string())
        .or_insert_with(|| json!([]));
    let allow_arr = match allow.as_array_mut() {
        Some(a) => a,
        None => return false,
    };

    let wanted = antigravity_permission_rules();
    let mut existing: std::collections::HashSet<String> = allow_arr
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();
    for rule in &wanted {
        if existing.insert(rule.clone()) {
            allow_arr.push(json!(rule));
        }
    }

    let json_str = match serde_json::to_string_pretty(&Value::Object(root)) {
        Ok(s) => s,
        Err(_) => return false,
    };
    crate::paths::atomic_write(&path, &json_str)
}

/// Remove hcom rules from agy settings.json. Cleans `permissions.allow` and
/// `permissions` if they become empty. Leaves the file otherwise untouched.
fn remove_antigravity_permissions() -> bool {
    remove_antigravity_permissions_at(&get_antigravity_settings_path())
}

fn remove_antigravity_permissions_at(path: &Path) -> bool {
    if !path.exists() {
        return true;
    }
    let content = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let mut val: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let root = match val.as_object_mut() {
        Some(o) => o,
        None => return false,
    };

    let wanted: std::collections::HashSet<String> =
        antigravity_permission_rules().into_iter().collect();

    let mut changed = false;
    if let Some(permissions) = root.get_mut("permissions").and_then(|v| v.as_object_mut()) {
        if let Some(allow) = permissions.get_mut("allow").and_then(|v| v.as_array_mut()) {
            let before = allow.len();
            allow.retain(|v| v.as_str().is_none_or(|s| !wanted.contains(s)));
            if allow.len() != before {
                changed = true;
            }
            if allow.is_empty() {
                permissions.remove("allow");
                changed = true;
            }
        }
        if permissions.is_empty() {
            root.remove("permissions");
            changed = true;
        }
    }

    if !changed {
        return true;
    }

    let json_str = match serde_json::to_string_pretty(&val) {
        Ok(s) => s,
        Err(_) => return false,
    };
    crate::paths::atomic_write(path, &json_str)
}

/// Check that every hcom rule we install is present in agy settings.json.
fn antigravity_permissions_complete(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(val) = serde_json::from_str::<Value>(&content) else {
        return false;
    };
    let Some(allow) = val
        .get("permissions")
        .and_then(|p| p.get("allow"))
        .and_then(|a| a.as_array())
    else {
        return false;
    };
    let installed: std::collections::HashSet<&str> =
        allow.iter().filter_map(|v| v.as_str()).collect();
    antigravity_permission_rules()
        .iter()
        .all(|rule| installed.contains(rule.as_str()))
}

/// agy Stop payloads that end a turn but not the session (do not soft-stop).
const AGY_TURN_END_REASONS: &[&str] = &["NO_TOOL_CALL", "NO_TOOL_CALLS"];

/// Antigravity Stop stdin uses `terminationReason`, not Gemini's `reason`.
pub(crate) fn sessionend_reason(raw: &Value) -> String {
    raw.get("terminationReason")
        .or_else(|| raw.get("reason"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "closed".to_string())
}

/// True when agy Stop is turn-end only (session continues), not tab/process teardown.
pub(crate) fn stop_should_skip_soft_finalize(raw: &Value) -> bool {
    if raw.get("fullyIdle").and_then(|v| v.as_bool()) == Some(true) {
        return true;
    }
    let reason = raw
        .get("terminationReason")
        .or_else(|| raw.get("reason"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    AGY_TURN_END_REASONS.contains(&reason.to_ascii_uppercase().as_str())
}

#[cfg(test)]
#[path = "antigravity_tests.rs"]
mod tests;
