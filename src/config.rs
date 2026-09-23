//! Configuration management — central config system used by all modules.
//!
//! Two config layers:
//! - `Config`: Runtime env vars (HCOM_DIR, HCOM_INSTANCE_NAME, etc.) — startup-only, used by router/client
//! - `HcomConfig`: User config from TOML + env vars — all 20 user-facing settings with validation

use regex::Regex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};

static RE_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9-]+$").unwrap());
static RE_PRESET_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-zA-Z0-9_]+$").unwrap());

use crate::paths;

/// Global configuration instance, lazily initialized and resettable for tests.
static CONFIG: Mutex<Option<Config>> = Mutex::new(None);

/// Configuration loaded from HCOM_* environment variables.
///
/// All environment variable access should go through this struct
/// rather than calling env::var directly.
#[derive(Clone, Debug)]
pub struct Config {
    /// HCOM directory (HCOM_DIR or ~/.hcom)
    pub hcom_dir: PathBuf,
    /// Instance name (HCOM_INSTANCE_NAME)
    pub instance_name: Option<String>,
    /// Process ID for daemon binding (HCOM_PROCESS_ID)
    pub process_id: Option<String>,
}

impl Config {
    /// Initialize global config from environment variables (call once at startup).
    /// Can be called multiple times - subsequent calls are no-ops.
    pub fn init() {
        let _ = Self::get();
    }

    /// Get global config, initializing it from the current environment if needed.
    pub fn get() -> Config {
        let mut config = CONFIG.lock().unwrap_or_else(|e| e.into_inner());
        if config.is_none() {
            *config = Some(Self::from_env());
        }
        config
            .clone()
            .expect("Config should be initialized before returning")
    }

    /// Reset global config (test-only).
    /// Allows tests to reinitialize config with different env vars.
    #[cfg(test)]
    pub fn reset() {
        *CONFIG.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    /// Load configuration from environment variables
    fn from_env() -> Self {
        use std::env;

        let env_map: HashMap<String, String> = env::vars().collect();
        let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let (hcom_dir, _) = paths::resolve_hcom_dir_from_env(&env_map, &cwd);

        // Unit tests must never inherit a real hcom data directory. Raw path
        // semantics are tested through `resolve_hcom_dir_from_env` directly, so
        // global Config accepts only roots a test fixture explicitly registered
        // as disposable — not merely "it lives under $TMPDIR", since a real DB
        // can sit under the temp tree too. Anything else redirects to a
        // process-local throwaway. Production builds do not compile this branch.
        //
        // Redirect rather than panic on an unregistered dir: countless tests
        // read Config with no HCOM_DIR set and no isolation installed, and must
        // land on a safe throwaway instead of aborting. Every explicit consumer
        // registers its root, so the fallback is a backstop, never the norm.
        #[cfg(test)]
        let hcom_dir = {
            if paths::test_roots::is_registered(&hcom_dir) {
                hcom_dir
            } else {
                test_default_hcom_dir()
            }
        };

        let instance_name = env::var("HCOM_INSTANCE_NAME")
            .ok()
            .filter(|s| !s.is_empty());

        let process_id = env::var("HCOM_PROCESS_ID").ok().filter(|s| !s.is_empty());

        Self {
            hcom_dir,
            instance_name,
            process_id,
        }
    }
}

/// Process-local fallback for unit tests that do not install an isolated
/// `HCOM_DIR`. Reusing one directory per test binary preserves Config's normal
/// process-wide semantics. Backed by a retained `TempDir` so it gets a unique,
/// uncontended name and is registered as a disposable root for the redirect.
#[cfg(test)]
fn test_default_hcom_dir() -> PathBuf {
    use std::sync::OnceLock;

    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::Builder::new()
            .prefix("hcom-test-default-")
            .tempdir()
            .expect("create test-default hcom dir");
        paths::test_roots::register(dir.path());
        dir
    })
    .path()
    .to_path_buf()
}

/// Bidirectional mapping: HcomConfig field name <-> TOML dotted path.
const TOML_KEY_MAP: &[(&str, &str)] = &[
    ("terminal", "terminal.active"),
    ("tag", "launch.tag"),
    ("hints", "launch.hints"),
    ("notes", "launch.notes"),
    ("subagent_timeout", "launch.subagent_timeout"),
    ("auto_subscribe", "launch.auto_subscribe"),
    ("claude_args", "launch.claude.args"),
    ("gemini_args", "launch.gemini.args"),
    ("gemini_system_prompt", "launch.gemini.system_prompt"),
    ("codex_args", "launch.codex.args"),
    ("codex_sandbox_mode", "launch.codex.sandbox_mode"),
    ("codex_system_prompt", "launch.codex.system_prompt"),
    ("opencode_args", "launch.opencode.args"),
    ("kilo_args", "launch.kilo.args"),
    ("pi_args", "launch.pi.args"),
    ("omp_args", "launch.omp.args"),
    ("cursor_args", "launch.cursor.args"),
    ("kimi_args", "launch.kimi.args"),
    ("copilot_args", "launch.copilot.args"),
    ("relay", "relay.url"),
    ("relay_id", "relay.id"),
    ("relay_token", "relay.token"),
    ("relay_psk", "relay.psk"),
    ("relay_enabled", "relay.enabled"),
    ("timeout", "preferences.timeout"),
    ("auto_approve", "preferences.auto_approve"),
    ("name_export", "preferences.name_export"),
    ("bigboss", "preferences.bigboss"),
    ("auto_trust_workspace", "launch.auto_trust_workspace"),
    ("title_mode", "terminal.title_mode"),
];

/// Mapping: HcomConfig field name -> HCOM_* env var key.
const FIELD_TO_ENV: &[(&str, &str)] = &[
    ("timeout", "HCOM_TIMEOUT"),
    ("subagent_timeout", "HCOM_SUBAGENT_TIMEOUT"),
    ("terminal", "HCOM_TERMINAL"),
    ("hints", "HCOM_HINTS"),
    ("notes", "HCOM_NOTES"),
    ("tag", "HCOM_TAG"),
    ("claude_args", "HCOM_CLAUDE_ARGS"),
    ("gemini_args", "HCOM_GEMINI_ARGS"),
    ("codex_args", "HCOM_CODEX_ARGS"),
    ("codex_sandbox_mode", "HCOM_CODEX_SANDBOX_MODE"),
    ("gemini_system_prompt", "HCOM_GEMINI_SYSTEM_PROMPT"),
    ("codex_system_prompt", "HCOM_CODEX_SYSTEM_PROMPT"),
    ("opencode_args", "HCOM_OPENCODE_ARGS"),
    ("kilo_args", "HCOM_KILO_ARGS"),
    ("pi_args", "HCOM_PI_ARGS"),
    ("omp_args", "HCOM_OMP_ARGS"),
    ("cursor_args", "HCOM_CURSOR_ARGS"),
    ("kimi_args", "HCOM_KIMI_ARGS"),
    ("copilot_args", "HCOM_COPILOT_ARGS"),
    ("relay", "HCOM_RELAY"),
    ("relay_id", "HCOM_RELAY_ID"),
    ("relay_token", "HCOM_RELAY_TOKEN"),
    // NOTE: `relay_psk` is deliberately NOT in FIELD_TO_ENV. `to_env_dict` feeds
    // `build_launch_env`, which injects these vars into every spawned agent
    // child. The PSK is forge/decrypt authority for the entire relay group —
    // it must never cross a process boundary via environment. Relay fields are
    // file-only on load already (see `is_relay_field` in `load_from_sources`),
    // so env-var override was never the mechanism for configuring the PSK.
    ("relay_enabled", "HCOM_RELAY_ENABLED"),
    ("auto_approve", "HCOM_AUTO_APPROVE"),
    ("auto_subscribe", "HCOM_AUTO_SUBSCRIBE"),
    ("name_export", "HCOM_NAME_EXPORT"),
    ("bigboss", "HCOM_BIGBOSS"),
    ("auto_trust_workspace", "HCOM_AUTO_TRUST_WORKSPACE"),
    ("title_mode", "HCOM_TITLE_MODE"),
];

/// Relay fields — file-only, no env var override.
const RELAY_FIELDS: &[&str] = &[
    "relay",
    "relay_id",
    "relay_token",
    "relay_psk",
    "relay_enabled",
];

/// Characters that are dangerous in terminal preset values (injection risk).
const TERMINAL_DANGEROUS_CHARS: &[char] = &['`', '$', ';', '|', '&', '\n', '\r'];

use crate::shared::terminal_presets::TERMINAL_PRESETS;

/// Valid codex sandbox modes.
pub const VALID_SANDBOX_MODES: &[&str] = &["workspace", "untrusted", "danger-full-access", "none"];

/// TOML file header comment.
const TOML_HEADER: &str = "\
# hcom configuration
# Help: hcom config --help
# Docs: hcom run docs
";

/// Get value from nested TOML table using dotted path (e.g., "launch.claude.args").
fn get_nested(table: &toml::Value, dotted_path: &str) -> Option<toml::Value> {
    let mut current = table;
    for part in dotted_path.split('.') {
        current = current.as_table()?.get(part)?;
    }
    Some(current.clone())
}

/// Set value in nested TOML table using dotted path, creating intermediates.
fn set_nested(table: &mut toml::Value, dotted_path: &str, value: toml::Value) {
    let parts: Vec<&str> = dotted_path.split('.').collect();
    let mut current = table;
    for &part in &parts[..parts.len() - 1] {
        let Some(tbl) = current.as_table_mut() else {
            return;
        };
        if !tbl.contains_key(part) {
            tbl.insert(part.to_string(), toml::Value::Table(toml::map::Map::new()));
        }
        let Some(next) = tbl.get_mut(part) else {
            return;
        };
        current = next;
    }
    if let Some(t) = current.as_table_mut() {
        t.insert(parts[parts.len() - 1].to_string(), value);
    }
}

/// Validation errors from HcomConfig construction.
#[derive(Debug, Clone)]
pub struct HcomConfigError {
    pub errors: HashMap<String, String>,
}

impl std::fmt::Display for HcomConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.errors.is_empty() {
            write!(f, "Invalid config")
        } else {
            writeln!(f, "Invalid config:")?;
            for msg in self.errors.values() {
                writeln!(f, "  - {msg}")?;
            }
            Ok(())
        }
    }
}

impl std::error::Error for HcomConfigError {}

/// HCOM user configuration with validation.
/// Load priority: env var → config.toml → defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct HcomConfig {
    pub timeout: i64,
    pub subagent_timeout: i64,
    pub terminal: String,
    pub hints: String,
    pub notes: String,
    pub tag: String,
    pub claude_args: String,
    pub gemini_args: String,
    pub codex_args: String,
    pub opencode_args: String,
    pub kilo_args: String,
    pub pi_args: String,
    /// Oh My Pi specific launch arguments
    pub omp_args: String,
    pub cursor_args: String,
    pub kimi_args: String,
    pub copilot_args: String,
    pub codex_sandbox_mode: String,
    pub gemini_system_prompt: String,
    pub codex_system_prompt: String,
    pub relay: String,
    pub relay_id: String,
    pub relay_token: String,
    pub relay_psk: String,
    pub relay_enabled: bool,
    pub auto_approve: bool,
    pub auto_subscribe: String,
    pub name_export: String,
    /// TUI coordinator name for the `B` shortcut. Default `"bigboss"`; empty
    /// resets to the default. Tagged and device-qualified names are allowed;
    /// whitespace and `"*"` are rejected.
    pub bigboss: String,
    pub auto_trust_workspace: bool,
    /// Terminal-title behavior: `"combined"` (default) shows
    /// `{icon} name - {tool's live title}`, `"label"` shows hcom's
    /// `{icon} name [tool]` only, `"off"` leaves the tool's own title untouched.
    /// See [`crate::shared::TitleMode`].
    pub title_mode: String,
}

impl Default for HcomConfig {
    fn default() -> Self {
        Self {
            timeout: 86400,
            subagent_timeout: 30,
            terminal: "default".to_string(),
            hints: String::new(),
            notes: String::new(),
            tag: String::new(),
            claude_args: String::new(),
            gemini_args: String::new(),
            codex_args: String::new(),
            opencode_args: String::new(),
            kilo_args: String::new(),
            pi_args: String::new(),
            omp_args: String::new(),
            cursor_args: String::new(),
            kimi_args: String::new(),
            copilot_args: String::new(),
            codex_sandbox_mode: "workspace".to_string(),
            gemini_system_prompt: String::new(),
            codex_system_prompt: String::new(),
            relay: String::new(),
            relay_id: String::new(),
            relay_token: String::new(),
            relay_psk: String::new(),
            relay_enabled: true,
            auto_approve: true,
            auto_subscribe: "collision".to_string(),
            name_export: String::new(),
            bigboss: "bigboss".to_string(),
            auto_trust_workspace: true,
            title_mode: "combined".to_string(),
        }
    }
}

impl HcomConfig {
    /// Normalize fields before validation (case normalization, legacy values).
    pub fn normalize(&mut self) {
        // Resolve old terminal casing (WezTerm→wezterm, Alacritty→alacritty)
        if self.terminal != "default" && self.terminal != "print" && self.terminal != "here" {
            self.terminal = normalize_terminal_case(&self.terminal);
        }
        // Empty coordinator name falls back to the default.
        if self.bigboss.trim().is_empty() {
            self.bigboss = "bigboss".to_string();
        }
    }

    /// Validate all fields, returning map of field → error message.
    /// Also normalizes fields (terminal case, etc.).
    pub fn collect_errors(&mut self) -> HashMap<String, String> {
        self.normalize();
        let mut errors: HashMap<String, String> = HashMap::new();

        // Validate timeout
        if !(1..=86400).contains(&self.timeout) {
            errors.insert(
                "timeout".into(),
                format!(
                    "timeout must be 1-86400 seconds (24 hours), got {}",
                    self.timeout
                ),
            );
        }

        // Validate subagent_timeout
        if !(1..=86400).contains(&self.subagent_timeout) {
            errors.insert(
                "subagent_timeout".into(),
                format!(
                    "subagent_timeout must be 1-86400 seconds, got {}",
                    self.subagent_timeout
                ),
            );
        }

        // Validate bigboss (already default-filled by normalize when empty).
        if self.bigboss.chars().any(char::is_whitespace) {
            errors.insert(
                "bigboss".into(),
                format!("bigboss cannot contain whitespace, got '{}'", self.bigboss),
            );
        } else if self.bigboss == "*" {
            errors.insert("bigboss".into(), "bigboss cannot be '*'".into());
        }

        // Validate terminal
        if self.terminal.is_empty() {
            errors.insert("terminal".into(), "terminal cannot be empty".into());
        } else if self.terminal != "default" && self.terminal != "print" && self.terminal != "here"
        {
            let platform = crate::shared::platform::platform_name();
            if let Some(error) = user_defined_preset_error(&self.terminal) {
                errors.insert(
                    "terminal".into(),
                    format!("invalid terminal preset '{}': {error}", self.terminal),
                );
            } else if is_user_defined_preset(&self.terminal) {
                // User TOML presets declare no platform and override any built-in
                // of the same name — exempt from the built-in platform gate.
            } else if is_known_terminal_preset(&self.terminal) {
                if !terminal_preset_supported_on(&self.terminal, platform) {
                    errors.insert(
                        "terminal".into(),
                        format!(
                            "terminal preset '{}' is not available on {}",
                            self.terminal, platform
                        ),
                    );
                }
            } else if !self.terminal.contains("{script}") {
                errors.insert(
                    "terminal".into(),
                    format!(
                        "terminal must be 'default', preset name, or custom command with {{script}}, got '{}'",
                        self.terminal
                    ),
                );
            }
        }

        // Validate tag (alphanumeric + hyphens only)
        if !self.tag.is_empty() && !RE_TAG.is_match(&self.tag) {
            errors.insert(
                "tag".into(),
                "tag can only contain letters, numbers, and hyphens".into(),
            );
        }

        if !crate::shared::VALID_TITLE_MODES.contains(&self.title_mode.as_str()) {
            errors.insert(
                "title_mode".into(),
                format!(
                    "title_mode must be one of: {}. Got '{}'",
                    crate::shared::VALID_TITLE_MODES.join(", "),
                    self.title_mode
                ),
            );
        }

        // Validate shell-quoted args fields
        for (field, value) in [
            ("claude_args", &self.claude_args),
            ("gemini_args", &self.gemini_args),
            ("codex_args", &self.codex_args),
            ("opencode_args", &self.opencode_args),
            ("kilo_args", &self.kilo_args),
            ("pi_args", &self.pi_args),
            ("omp_args", &self.omp_args),
            ("cursor_args", &self.cursor_args),
            ("kimi_args", &self.kimi_args),
            ("copilot_args", &self.copilot_args),
        ] {
            if !value.is_empty()
                && let Err(e) = shell_words::split(value)
            {
                errors.insert(
                    field.into(),
                    format!("{field} contains invalid shell quoting: {e}"),
                );
            }
        }

        // Validate codex_sandbox_mode
        if !VALID_SANDBOX_MODES.contains(&self.codex_sandbox_mode.as_str()) {
            errors.insert(
                "codex_sandbox_mode".into(),
                format!(
                    "codex_sandbox_mode must be one of {:?}, got '{}'",
                    VALID_SANDBOX_MODES, self.codex_sandbox_mode
                ),
            );
        }

        // Validate auto_subscribe (comma-separated alphanumeric/underscore preset names)
        if !self.auto_subscribe.is_empty() {
            for preset in self.auto_subscribe.split(',') {
                let preset = preset.trim();
                if !preset.is_empty() && !RE_PRESET_NAME.is_match(preset) {
                    errors.insert(
                        "auto_subscribe".into(),
                        format!(
                            "auto_subscribe preset '{preset}' contains invalid characters (alphanumeric/underscore only)"
                        ),
                    );
                }
            }
        }

        errors
    }

    /// Validate and return list of error messages.
    pub fn validate(&mut self) -> Vec<String> {
        self.collect_errors().into_values().collect()
    }

    /// Get a field value by name (returns string representation).
    pub fn get_field(&self, field: &str) -> Option<String> {
        match field {
            "timeout" => Some(self.timeout.to_string()),
            "subagent_timeout" => Some(self.subagent_timeout.to_string()),
            "terminal" => Some(self.terminal.clone()),
            "hints" => Some(self.hints.clone()),
            "notes" => Some(self.notes.clone()),
            "tag" => Some(self.tag.clone()),
            "claude_args" => Some(self.claude_args.clone()),
            "gemini_args" => Some(self.gemini_args.clone()),
            "codex_args" => Some(self.codex_args.clone()),
            "opencode_args" => Some(self.opencode_args.clone()),
            "kilo_args" => Some(self.kilo_args.clone()),
            "pi_args" => Some(self.pi_args.clone()),
            "omp_args" => Some(self.omp_args.clone()),
            "cursor_args" => Some(self.cursor_args.clone()),
            "kimi_args" => Some(self.kimi_args.clone()),
            "copilot_args" => Some(self.copilot_args.clone()),
            "codex_sandbox_mode" => Some(self.codex_sandbox_mode.clone()),
            "gemini_system_prompt" => Some(self.gemini_system_prompt.clone()),
            "codex_system_prompt" => Some(self.codex_system_prompt.clone()),
            "relay" => Some(self.relay.clone()),
            "relay_id" => Some(self.relay_id.clone()),
            "relay_token" => Some(self.relay_token.clone()),
            "relay_psk" => Some(self.relay_psk.clone()),
            "relay_enabled" => Some(if self.relay_enabled { "1" } else { "0" }.into()),
            "auto_approve" => Some(if self.auto_approve { "1" } else { "0" }.into()),
            "auto_subscribe" => Some(self.auto_subscribe.clone()),
            "name_export" => Some(self.name_export.clone()),
            "bigboss" => Some(self.bigboss.clone()),
            "auto_trust_workspace" => {
                Some(if self.auto_trust_workspace { "1" } else { "0" }.into())
            }
            "title_mode" => Some(self.title_mode.clone()),
            _ => None,
        }
    }

    /// Set a field value by name. Returns Err if field unknown or value invalid type.
    pub fn set_field(&mut self, field: &str, value: &str) -> Result<(), String> {
        match field {
            "timeout" => {
                self.timeout = value
                    .parse()
                    .map_err(|_| format!("timeout must be an integer, got '{value}'"))?;
            }
            "subagent_timeout" => {
                self.subagent_timeout = value
                    .parse()
                    .map_err(|_| format!("subagent_timeout must be an integer, got '{value}'"))?;
            }
            "terminal" => self.terminal = value.to_string(),
            "hints" => self.hints = value.to_string(),
            "notes" => self.notes = value.to_string(),
            "tag" => self.tag = value.to_string(),
            "claude_args" => self.claude_args = value.to_string(),
            "gemini_args" => self.gemini_args = value.to_string(),
            "codex_args" => self.codex_args = value.to_string(),
            "opencode_args" => self.opencode_args = value.to_string(),
            "kilo_args" => self.kilo_args = value.to_string(),
            "pi_args" => self.pi_args = value.to_string(),
            "omp_args" => self.omp_args = value.to_string(),
            "cursor_args" => self.cursor_args = value.to_string(),
            "kimi_args" => self.kimi_args = value.to_string(),
            "copilot_args" => self.copilot_args = value.to_string(),
            "codex_sandbox_mode" => {
                // Normalize legacy value
                self.codex_sandbox_mode = if value == "full-auto" {
                    "workspace".to_string()
                } else {
                    value.to_string()
                };
            }
            "gemini_system_prompt" => self.gemini_system_prompt = value.to_string(),
            "codex_system_prompt" => self.codex_system_prompt = value.to_string(),
            "relay" => self.relay = value.to_string(),
            "relay_id" => self.relay_id = value.to_string(),
            "relay_token" => self.relay_token = value.to_string(),
            "relay_psk" => self.relay_psk = value.to_string(),
            "relay_enabled" => self.relay_enabled = !is_falsy(value),
            "auto_approve" => self.auto_approve = !is_falsy(value),
            "auto_subscribe" => self.auto_subscribe = value.to_string(),
            "name_export" => self.name_export = value.to_string(),
            "bigboss" => self.bigboss = value.to_string(),
            "auto_trust_workspace" => self.auto_trust_workspace = !is_falsy(value),
            // Stored leniently; `TitleMode::from_config` maps unknown → default.
            // The CLI set path (`config_set_at_path`) validates against
            // `VALID_TITLE_MODES` before this is ever written to the file.
            "title_mode" => self.title_mode = value.to_string(),
            _ => return Err(format!("unknown field: {field}")),
        }
        Ok(())
    }

    /// Effective HCOM_TIMEOUT (idle poll timeout for non-PTY instances), falling
    /// back to 120s if config can't be loaded. Used both by the Stop-hook poll
    /// fallback and by registration so freshly created rows already carry the
    /// resolved value instead of relying on the (never-NULL) schema default.
    pub fn effective_timeout() -> i64 {
        Self::load(None).ok().map(|c| c.timeout).unwrap_or(120)
    }

    /// Load config with precedence: env var → config.toml → defaults.
    ///
    /// `env_override`: If Some, use this map for env var lookups instead of std::env.
    /// Used in daemon mode where os.environ is stale.
    pub fn load(env_override: Option<&HashMap<String, String>>) -> Result<Self, HcomConfigError> {
        let toml_path = paths::config_toml_path();

        if !toml_path.exists() {
            let hcom_dir = &Config::get().hcom_dir;
            let config_env_path = hcom_dir.join("config.env");
            if config_env_path.exists() {
                // Legacy config.env exists — migration to config.toml not yet done.
                // Don't write default config.toml here or we'd silently lose the
                // user's settings. Load returns defaults for this invocation.
            } else {
                // No config at all — write defaults
                let _ = write_default_config();
            }
        }

        // Parse config.toml
        let file_config = if toml_path.exists() {
            load_toml_config(&toml_path)
        } else {
            HashMap::new()
        };

        Self::load_from_sources(&file_config, env_override)
    }

    /// Load from pre-parsed TOML values + env. Separated for testability.
    fn load_from_sources(
        file_config: &HashMap<String, TomlFieldValue>,
        env_override: Option<&HashMap<String, String>>,
    ) -> Result<Self, HcomConfigError> {
        let mut config = HcomConfig::default();

        let is_relay_field = |field: &str| -> bool { RELAY_FIELDS.contains(&field) };

        // Helper: get value with precedence env → file
        let get_var = |field: &str| -> Option<TomlFieldValue> {
            let env_key = FIELD_TO_ENV
                .iter()
                .find(|&&(f, _)| f == field)
                .map(|&(_, e)| e);

            // Relay fields are file-only (no env override)
            if let Some(env_key) = env_key
                && !is_relay_field(field)
            {
                let env_val = if let Some(overrides) = env_override {
                    overrides.get(env_key).cloned()
                } else {
                    std::env::var(env_key).ok()
                };
                if let Some(val) = env_val {
                    return Some(TomlFieldValue::Str(val));
                }
            }

            file_config.get(field).cloned()
        };

        // Load integer fields
        for int_field in &["timeout", "subagent_timeout"] {
            if let Some(val) = get_var(int_field) {
                match val {
                    TomlFieldValue::Int(i) => {
                        let _ = config.set_field(int_field, &i.to_string());
                    }
                    TomlFieldValue::Str(s) if !s.is_empty() => {
                        if let Ok(i) = s.parse::<i64>() {
                            let _ = config.set_field(int_field, &i.to_string());
                        }
                        // Invalid int: silently use default
                    }
                    _ => {}
                }
            }
        }

        // Load string fields
        let str_fields = [
            "terminal",
            "hints",
            "notes",
            "tag",
            "claude_args",
            "gemini_args",
            "codex_args",
            "opencode_args",
            "kilo_args",
            "pi_args",
            "cursor_args",
            "copilot_args",
            "codex_sandbox_mode",
            "gemini_system_prompt",
            "codex_system_prompt",
            "auto_subscribe",
            "name_export",
            "bigboss",
            "title_mode",
        ];
        for str_field in &str_fields {
            if let Some(val) = get_var(str_field) {
                let s = val.as_string();
                // terminal, codex_sandbox_mode and bigboss: skip empty (use default)
                if (*str_field == "terminal"
                    || *str_field == "codex_sandbox_mode"
                    || *str_field == "bigboss")
                    && s.is_empty()
                {
                    continue;
                }
                let _ = config.set_field(str_field, &s);
            }
        }

        // Load boolean fields
        for bool_field in &["relay_enabled", "auto_approve", "auto_trust_workspace"] {
            if let Some(val) = get_var(bool_field) {
                match val {
                    TomlFieldValue::Bool(b) => {
                        let _ = config.set_field(bool_field, if b { "1" } else { "0" });
                    }
                    TomlFieldValue::Int(i) => {
                        let _ = config.set_field(bool_field, if i == 0 { "0" } else { "1" });
                    }
                    TomlFieldValue::Str(s) => {
                        let _ = config.set_field(bool_field, if is_falsy(&s) { "0" } else { "1" });
                    }
                }
            }
        }

        // Load relay string fields (file-only, already handled by get_var)
        for relay_field in &["relay", "relay_id", "relay_token", "relay_psk"] {
            if let Some(val) = get_var(relay_field) {
                let _ = config.set_field(relay_field, &val.as_string());
            }
        }

        // Validate
        let errors = config.collect_errors();
        if !errors.is_empty() {
            return Err(HcomConfigError { errors });
        }

        Ok(config)
    }

    /// Convert to HCOM_* env var dict (for persistence/display). Relay secret
    /// material (the PSK) is never emitted here — see `FIELD_TO_ENV` for why.
    pub fn to_env_dict(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for &(field, env_key) in FIELD_TO_ENV {
            if field == "relay_psk" {
                continue;
            }
            if let Some(val) = self.get_field(field) {
                map.insert(env_key.to_string(), val);
            }
        }
        map
    }

    /// Build from HCOM_* env var dict. Returns validated config.
    pub fn from_env_dict(data: &HashMap<String, String>) -> Result<Self, HcomConfigError> {
        let mut config = HcomConfig::default();
        let mut errors: HashMap<String, String> = HashMap::new();

        // Build reverse map: HCOM_* key -> field name
        let env_to_field: HashMap<&str, &str> = FIELD_TO_ENV.iter().map(|&(f, e)| (e, f)).collect();

        for (env_key, value) in data {
            if let Some(&field) = env_to_field.get(env_key.as_str())
                && let Err(e) = config.set_field(field, value)
            {
                errors.insert(field.to_string(), e);
            }
        }

        if !errors.is_empty() {
            return Err(HcomConfigError { errors });
        }

        // Run validation
        let validation_errors = config.collect_errors();
        if !validation_errors.is_empty() {
            return Err(HcomConfigError {
                errors: validation_errors,
            });
        }

        Ok(config)
    }

    /// Convert to nested TOML-ready table.
    pub fn to_toml_table(&self) -> toml::Value {
        let mut table = default_toml_structure();
        for &(field, toml_path) in TOML_KEY_MAP {
            if let Some(val) = self.get_field(field) {
                // Determine TOML value type from the default structure
                let default_val = get_nested(&table, toml_path);
                let toml_val = match default_val {
                    Some(toml::Value::Boolean(_)) => toml::Value::Boolean(!is_falsy(&val)),
                    Some(toml::Value::Integer(_)) => toml::Value::Integer(val.parse().unwrap_or(0)),
                    _ => toml::Value::String(val),
                };
                set_nested(&mut table, toml_path, toml_val);
            }
        }
        table
    }
}

/// Typed value from TOML parsing (preserves original type for coercion).
#[derive(Clone, Debug)]
pub(crate) enum TomlFieldValue {
    Str(String),
    Int(i64),
    Bool(bool),
}

impl TomlFieldValue {
    fn as_string(&self) -> String {
        match self {
            TomlFieldValue::Str(s) => s.clone(),
            TomlFieldValue::Int(i) => i.to_string(),
            TomlFieldValue::Bool(b) => {
                if *b {
                    "1".to_string()
                } else {
                    "0".to_string()
                }
            }
        }
    }
}

/// Load config.toml and return flat map of field name → typed value.
/// Includes terminal dangerous-char validation.
pub fn load_toml_config(path: &std::path::Path) -> HashMap<String, TomlFieldValue> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };

    let raw: toml::Value = match content.parse::<toml::Table>() {
        Ok(t) => toml::Value::Table(t),
        Err(e) => {
            eprintln!(
                "Warning: Failed to parse {}: {e} — using defaults",
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            );
            return HashMap::new();
        }
    };

    let mut result = HashMap::new();
    for &(field_name, toml_path) in TOML_KEY_MAP {
        if let Some(val) = get_nested(&raw, toml_path) {
            let typed = match &val {
                toml::Value::String(s) => TomlFieldValue::Str(s.clone()),
                toml::Value::Integer(i) => TomlFieldValue::Int(*i),
                toml::Value::Boolean(b) => TomlFieldValue::Bool(*b),
                _ => continue,
            };
            result.insert(field_name.to_string(), typed);
        }
    }

    // Terminal dangerous-char validation
    if let Some(TomlFieldValue::Str(terminal_val)) = result.get("terminal")
        && terminal_val
            .chars()
            .any(|c| TERMINAL_DANGEROUS_CHARS.contains(&c))
    {
        let bad_chars: Vec<String> = TERMINAL_DANGEROUS_CHARS
            .iter()
            .filter(|&&c| terminal_val.contains(c))
            .map(|c| format!("{c:?}"))
            .collect();
        eprintln!(
            "Warning: Unsafe characters in terminal.active ({}), ignoring custom terminal command",
            bad_chars.join(", ")
        );
        result.remove("terminal");
    }

    result
}

/// Write config.toml from HcomConfig using toml_edit to preserve comments and formatting.
/// If the file already exists, parses it and surgically updates only changed keys.
/// If the file doesn't exist, writes a fresh default with the header comment.
pub fn save_toml_config(config: &HcomConfig, presets: Option<&toml::Value>) -> std::io::Result<()> {
    use toml_edit::DocumentMut;

    let toml_path = paths::config_toml_path();

    // Ensure parent dir exists
    if let Some(parent) = toml_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Load existing document or create fresh one with header
    let mut doc: DocumentMut = if toml_path.exists() {
        let existing = std::fs::read_to_string(&toml_path)?;
        existing
            .parse::<DocumentMut>()
            .unwrap_or_else(|_| format!("{TOML_HEADER}\n").parse::<DocumentMut>().unwrap())
    } else {
        format!("{TOML_HEADER}\n").parse::<DocumentMut>().unwrap()
    };

    // Update each config key in the document
    for &(field, toml_path_str) in TOML_KEY_MAP {
        if let Some(val) = config.get_field(field) {
            set_nested_edit(&mut doc, toml_path_str, &val);
        }
    }

    // Merge terminal presets if provided
    if let Some(presets_val) = presets {
        // Convert toml::Value presets to toml_edit items
        if let Some(presets_table) = presets_val.as_table() {
            ensure_edit_table(&mut doc, "terminal");
            let Some(terminal) = doc["terminal"].as_table_mut() else {
                return Ok(());
            };
            // Parse via a wrapper doc so we get a proper nested table, not a document root
            let wrapper_str = format!(
                "[presets]\n{}",
                toml::to_string_pretty(&toml::Value::Table(presets_table.clone()))
                    .unwrap_or_default()
            );
            if let Ok(wrapper_doc) = wrapper_str.parse::<DocumentMut>()
                && let Some(item) = wrapper_doc
                    .as_item()
                    .as_table()
                    .and_then(|t| t.get("presets"))
            {
                terminal.insert("presets", item.clone());
            }
        }
    }

    write_config_toml_path(&toml_path, &doc.to_string())
}

/// Set a value in a toml_edit document using a dotted path, creating intermediate tables.
fn set_nested_edit(doc: &mut toml_edit::DocumentMut, dotted_path: &str, value: &str) {
    let parts: Vec<&str> = dotted_path.split('.').collect();

    // Build the default structure to determine expected type
    let defaults = default_toml_structure();
    let default_val = get_nested(&defaults, dotted_path);

    // Convert string value to the correct toml_edit type
    let edit_value: toml_edit::Value = match default_val {
        Some(toml::Value::Boolean(_)) => toml_edit::value(!is_falsy(value)).into_value().unwrap(),
        Some(toml::Value::Integer(_)) => toml_edit::value(value.parse::<i64>().unwrap_or(0))
            .into_value()
            .unwrap(),
        _ => toml_edit::value(value).into_value().unwrap(),
    };

    // Navigate/create intermediate tables
    match parts.len() {
        1 => {
            doc[parts[0]] = toml_edit::Item::Value(edit_value);
        }
        2 => {
            ensure_edit_table(doc, parts[0]);
            doc[parts[0]][parts[1]] = toml_edit::Item::Value(edit_value);
        }
        3 => {
            ensure_edit_table(doc, parts[0]);
            let Some(t) = doc[parts[0]].as_table_mut() else {
                return;
            };
            if t.get(parts[1]).is_none() || !t[parts[1]].is_table() {
                t.insert(parts[1], toml_edit::Item::Table(toml_edit::Table::new()));
            }
            doc[parts[0]][parts[1]][parts[2]] = toml_edit::Item::Value(edit_value);
        }
        _ => {} // Deeper nesting not used in current config
    }
}

/// Ensure a top-level key exists as a table in the document.
fn ensure_edit_table(doc: &mut toml_edit::DocumentMut, key: &str) {
    if doc.get(key).is_none() || !doc[key].is_table() {
        doc[key] = toml_edit::Item::Table(toml_edit::Table::new());
    }
}

/// Load terminal presets from config.toml [terminal.presets.*] section.
pub fn load_toml_presets(path: &std::path::Path) -> Option<toml::Value> {
    let content = std::fs::read_to_string(path).ok()?;
    let raw: toml::Value = toml::Value::Table(content.parse::<toml::Table>().ok()?);
    let terminal = raw.as_table()?.get("terminal")?.as_table()?;
    let presets = terminal.get("presets")?;
    if presets.is_table() {
        Some(presets.clone())
    } else {
        None
    }
}

/// Build the canonical default TOML structure
fn default_toml_structure() -> toml::Value {
    let toml_str = r#"[terminal]
active = "default"
title_mode = "combined"

[relay]
url = ""
id = ""
token = ""
psk = ""
enabled = true

[launch]
tag = ""
hints = ""
notes = ""
subagent_timeout = 30
auto_subscribe = "collision"
auto_trust_workspace = true

[launch.claude]
args = ""

[launch.gemini]
args = ""
system_prompt = ""

[launch.codex]
args = ""
sandbox_mode = "workspace"
system_prompt = ""

[launch.opencode]
args = ""

[launch.kilo]
args = ""

[launch.pi]
args = ""

[launch.omp]
args = ""

[launch.cursor]
args = ""

[launch.copilot]
args = ""

[preferences]
timeout = 86400
auto_approve = true
name_export = ""
"#;
    toml::Value::Table(toml_str.parse::<toml::Table>().unwrap())
}

/// Check if a string value is falsy
/// Check if a terminal name matches a known built-in preset (case-insensitive).
/// Public alias for use by status command.
pub fn is_known_terminal_preset_pub(name: &str) -> bool {
    is_known_terminal_preset(name)
}

/// Check if a terminal name matches a known built-in preset (case-insensitive).
fn is_known_terminal_preset(name: &str) -> bool {
    TERMINAL_PRESETS
        .iter()
        .any(|(p, _)| p.eq_ignore_ascii_case(name))
}

/// True if `name` is a built-in preset supported on `platform`
/// ("Darwin"/"Linux"/"Windows", see `crate::shared::platform::platform_name`).
pub fn terminal_preset_supported_on(name: &str, platform: &str) -> bool {
    TERMINAL_PRESETS
        .iter()
        .any(|(p, preset)| p.eq_ignore_ascii_case(name) && preset.platforms.contains(&platform))
}

/// Resolve old casing to canonical preset name (e.g., "WezTerm" → "wezterm").
/// Returns the canonical name if matched, otherwise returns the input unchanged.
fn normalize_terminal_case(name: &str) -> String {
    for &(preset, _) in TERMINAL_PRESETS.iter() {
        if preset.eq_ignore_ascii_case(name) {
            return preset.to_string();
        }
    }
    name.to_string()
}

fn user_defined_preset_error(name: &str) -> Option<String> {
    let presets = load_toml_presets(&paths::config_toml_path())?;
    let (_, value) = presets
        .as_table()?
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))?;
    let preset = match value.as_table() {
        Some(preset) => preset,
        None => return Some("expected a table".to_string()),
    };
    for field in ["open", "close"] {
        if let Some(value) = preset.get(field)
            && let Err(error) = toml_val_to_argv(value)
        {
            return Some(format!("{field}: {error}"));
        }
    }
    None
}

/// Check if a terminal name matches a user-defined preset in config.toml.
pub fn is_user_defined_preset(name: &str) -> bool {
    let toml_path = paths::config_toml_path();
    if let Some(presets_val) = load_toml_presets(&toml_path)
        && let Some(table) = presets_val.as_table()
    {
        return table.iter().any(|(key, value)| {
            key.eq_ignore_ascii_case(name)
                && value.as_table().is_some_and(|preset| {
                    preset.get("open").map(toml_val_to_argv).transpose().is_ok()
                        && preset
                            .get("close")
                            .map(toml_val_to_argv)
                            .transpose()
                            .is_ok()
                })
        });
    }
    false
}

/// Get the pane_id_env for a preset, checking TOML overrides then built-in defaults.
pub fn get_merged_preset_pane_id_env(name: &str) -> Option<String> {
    get_merged_preset(name).and_then(|p| p.pane_id_env)
}

/// Environment variables that identify a terminal-local pane/window/workspace.
///
/// New-window runner sidecars must not replay these values from the parent:
/// the terminal backend supplies fresh identity to the child. Include both
/// built-in detection variables and user-configured `pane_id_env` values.
pub fn pane_identity_env_vars() -> std::collections::HashSet<String> {
    pane_identity_env_vars_from_path(&paths::config_toml_path())
}

fn pane_identity_env_vars_from_path(
    toml_path: &std::path::Path,
) -> std::collections::HashSet<String> {
    let mut vars = crate::shared::terminal_presets::TERMINAL_ENV_MAP
        .iter()
        .map(|(env_var, _)| (*env_var).to_string())
        .collect::<std::collections::HashSet<_>>();

    if let Some(presets) = load_toml_presets(toml_path).and_then(|value| value.as_table().cloned())
    {
        vars.extend(presets.values().filter_map(|value| {
            value
                .as_table()
                .and_then(|preset| preset.get("pane_id_env"))
                .and_then(|value| value.as_str())
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        }));
    }

    vars
}

/// Get a fully merged terminal preset: TOML overrides on top of built-in defaults.
///
/// Returns None if the name matches neither a TOML preset nor a built-in preset.
pub fn get_merged_preset(name: &str) -> Option<MergedPreset> {
    let toml_path = paths::config_toml_path();
    let toml_preset = load_toml_presets(&toml_path).and_then(|presets| {
        let table = presets.as_table()?;
        let val = table
            .get(name)
            .or_else(|| {
                table
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .map(|(_, v)| v)
            })?
            .as_table()?;
        let open_result = val.get("open").map(toml_val_to_argv).transpose();
        let close_result = val.get("close").map(toml_val_to_argv).transpose();
        match (open_result, close_result) {
            (Err(e), _) | (_, Err(e)) => {
                eprintln!("Warning: skipping custom terminal preset {name:?}: {e}");
                None
            }
            (Ok(open), Ok(close)) => Some(TomlPresetFields {
                binary: val
                    .get("binary")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                app_name: val
                    .get("app_name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                open: open.flatten(),
                close: close.flatten(),
                pane_id_env: val
                    .get("pane_id_env")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
            }),
        }
    });

    let builtin = crate::shared::get_terminal_preset(name);

    match (&toml_preset, &builtin) {
        (None, None) => None,
        _ => {
            // Built-in argv templates, lowered to owned Vec<String> per platform.
            let argv_vec = |sel: Option<crate::shared::terminal_presets::ArgvTemplate>| {
                sel.map(|t| t.iter().map(|s| s.to_string()).collect::<Vec<String>>())
            };
            let b_open = builtin.map(|b| b.open);
            let b_close = builtin.map(|b| b.close);
            let b_binary = builtin.and_then(|b| b.binary);
            let b_app = builtin.and_then(|b| b.app_name);
            let b_pane_env = builtin.and_then(|b| b.pane_id_env);

            let t = toml_preset.as_ref();

            // A TOML `open`/`close` override (string or array) replaces the
            // built-in on BOTH platforms — TOML custom presets have no separate
            // Windows slot, so the array form is the Windows escape hatch (it
            // can carry literal Windows paths without shell mangling).
            let toml_open = t.and_then(|t| t.open.clone());
            let toml_close = t.and_then(|t| t.close.clone());

            let (open, open_windows) = match toml_open {
                Some(o) => (o, None),
                None => (
                    argv_vec(b_open.and_then(|o| o.default)).unwrap_or_default(),
                    argv_vec(b_open.and_then(|o| o.windows)),
                ),
            };
            let (close, close_windows) = match toml_close {
                Some(c) => (Some(c), None),
                None => (
                    argv_vec(b_close.and_then(|c| c.default)),
                    argv_vec(b_close.and_then(|c| c.windows)),
                ),
            };

            Some(MergedPreset {
                open,
                open_windows,
                close,
                close_windows,
                binary: t
                    .and_then(|t| t.binary.clone())
                    .or_else(|| b_binary.map(|s| s.to_string())),
                app_name: t
                    .and_then(|t| t.app_name.clone())
                    .or_else(|| b_app.map(|s| s.to_string())),
                pane_id_env: t
                    .and_then(|t| t.pane_id_env.clone())
                    .or_else(|| b_pane_env.map(|s| s.to_string())),
            })
        }
    }
}

/// Convert a TOML preset `open`/`close` value into an argv vector.
///
/// Accepts BOTH forms:
/// - Array: each element must be a string; non-string elements are an error.
///   Literal Windows paths like `C:\Users\x\s.ps1` survive intact — this is
///   the recommended escape hatch for custom presets.
/// - String (legacy): tokenized once via the double-quote-aware
///   `args_common::shell_split`. Backslashes are consumed by that tokenizer, so
///   the array form is preferred for Windows paths. A `\` in the string triggers
///   a warning so users can migrate to the array form.
///
/// Returns `Ok(None)` for an empty string/array (treated as "unset").
/// Returns `Err` on non-string array elements or invalid shell quoting — the
/// caller should treat this as a configuration error rather than falling back
/// to a built-in preset.
fn toml_val_to_argv(v: &toml::Value) -> Result<Option<Vec<String>>, String> {
    match v {
        toml::Value::Array(items) => {
            if items.is_empty() {
                return Ok(None);
            }
            let mut argv = Vec::with_capacity(items.len());
            for (i, e) in items.iter().enumerate() {
                match e.as_str() {
                    Some(s) => argv.push(s.to_string()),
                    None => {
                        return Err(format!(
                            "element [{}] is not a string (got {}); use quoted strings",
                            i,
                            e.type_str()
                        ));
                    }
                }
            }
            Ok(Some(argv))
        }
        toml::Value::String(s) => {
            if s.is_empty() {
                return Ok(None);
            }
            if s.contains('\\') {
                eprintln!(
                    "Warning: custom terminal preset command contains backslashes; \
                     use the array form to avoid shell tokenization issues on Windows: {s:?}"
                );
            }
            match crate::tools::args_common::shell_split(s, cfg!(windows)) {
                Ok(argv) if !argv.is_empty() => Ok(Some(argv)),
                Ok(_) => Ok(None),
                Err(e) => Err(format!("invalid quoting in preset command: {e}")),
            }
        }
        _ => Err(format!(
            "expected a string or array of strings, got {}",
            v.type_str()
        )),
    }
}

/// Parsed TOML preset fields (all optional — overlay on built-in).
struct TomlPresetFields {
    binary: Option<String>,
    app_name: Option<String>,
    open: Option<Vec<String>>,
    close: Option<Vec<String>>,
    pane_id_env: Option<String>,
}

/// Fully merged terminal preset (TOML + built-in), as argument vectors.
#[derive(Debug, Clone)]
pub struct MergedPreset {
    /// Default (Unix / fallback) open argv.
    pub open: Vec<String>,
    /// Windows-specific open argv override (None ⇒ use `open`).
    pub open_windows: Option<Vec<String>>,
    /// Default (Unix / fallback) close argv (None ⇒ no close API).
    pub close: Option<Vec<String>>,
    /// Windows-specific close argv override (None ⇒ use `close`).
    pub close_windows: Option<Vec<String>>,
    pub binary: Option<String>,
    pub app_name: Option<String>,
    pub pane_id_env: Option<String>,
}

impl MergedPreset {
    /// Open argv for the given platform (Windows falls back to the default).
    pub fn open_argv(&self, is_windows: bool) -> Vec<String> {
        if is_windows {
            self.open_windows
                .clone()
                .unwrap_or_else(|| self.open.clone())
        } else {
            self.open.clone()
        }
    }

    /// Close argv for the given platform (Windows falls back to the default).
    pub fn close_argv(&self, is_windows: bool) -> Option<Vec<String>> {
        if is_windows {
            self.close_windows.clone().or_else(|| self.close.clone())
        } else {
            self.close.clone()
        }
    }

    /// Whether a close API exists for the given platform (no clone).
    pub fn has_close(&self, is_windows: bool) -> bool {
        if is_windows {
            self.close_windows.is_some() || self.close.is_some()
        } else {
            self.close.is_some()
        }
    }
}

fn is_falsy(s: &str) -> bool {
    matches!(s, "0" | "false" | "False" | "no" | "off" | "")
}

/// Structured snapshot of config state for load/save operations.
#[derive(Clone, Debug)]
pub struct ConfigSnapshot {
    pub core: HcomConfig,
}

/// Load config snapshot from files (no env overrides — file contents only).
pub fn load_config_snapshot() -> ConfigSnapshot {
    let toml_path = paths::config_toml_path();

    if !toml_path.exists() {
        // Check for legacy config.env before writing defaults — don't overwrite
        // user settings that haven't been migrated yet.
        let config_env_path = Config::get().hcom_dir.join("config.env");
        if !config_env_path.exists() {
            let _ = write_default_config();
        }
    }

    let file_config = if toml_path.exists() {
        load_toml_config(&toml_path)
    } else {
        HashMap::new()
    };

    // Build HcomConfig from file values only (no env)
    let core = match HcomConfig::load_from_sources(&file_config, Some(&HashMap::new())) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            HcomConfig::default()
        }
    };

    ConfigSnapshot { core }
}

/// Write default config.toml, plus the env file unless one already exists
/// (a pre-seeded passthrough must survive first-run config creation).
pub fn write_default_config() -> std::io::Result<()> {
    let config = HcomConfig::default();
    save_toml_config(&config, None)?;
    if Config::get().hcom_dir.join("env").exists() {
        return Ok(());
    }
    save_env_file(&HashMap::new())
}

const ENV_HEADER: &str = "# Env vars passed through to agents (e.g. ANTHROPIC_MODEL=...)\n";
const DEFAULT_ENV_VARS: &[&str] = &[
    "ANTHROPIC_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL",
    "GEMINI_MODEL",
];

/// Load non-HCOM env vars from env file.
pub fn load_env_extras(path: &std::path::Path) -> HashMap<String, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };

    let mut result = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if !key.is_empty() && !key.starts_with("HCOM_") {
                result.insert(key.to_string(), parse_env_value(value));
            }
        }
    }
    result
}

/// Write env passthrough file (non-HCOM vars only).
pub fn save_env_file(extras: &HashMap<String, String>) -> std::io::Result<()> {
    let env_path = Config::get().hcom_dir.join("env");

    if let Some(parent) = env_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut lines = vec![ENV_HEADER.to_string()];

    // Always include default placeholders
    let mut all_keys: Vec<String> = DEFAULT_ENV_VARS.iter().map(|s| s.to_string()).collect();
    for key in extras.keys() {
        if !all_keys.contains(key) && !key.starts_with("HCOM_") {
            all_keys.push(key.clone());
        }
    }

    for key in &all_keys {
        if key.starts_with("HCOM_") {
            continue;
        }
        let value = extras.get(key.as_str()).map(|s| s.as_str()).unwrap_or("");
        let formatted = format_env_value(value);
        if formatted.is_empty() {
            lines.push(format!("{key}="));
        } else {
            lines.push(format!("{key}={formatted}"));
        }
    }

    let content = lines.join("\n") + "\n";
    atomic_write(&env_path, &content)
}

/// Parse ENV file value with proper quote and escape handling.
fn parse_env_value(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }

    // Double-quoted: unescape
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        let inner = &value[1..value.len() - 1];
        return inner
            .replace("\\\\", "\x00")
            .replace("\\n", "\n")
            .replace("\\t", "\t")
            .replace("\\\"", "\"")
            .replace('\x00', "\\");
    }

    // Single-quoted: literal
    if value.len() >= 2 && value.starts_with('\'') && value.ends_with('\'') {
        return value[1..value.len() - 1].to_string();
    }

    value.to_string()
}

/// Format value for ENV file with proper quoting (inverse of parse_env_value).
fn format_env_value(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }

    let needs_quoting = value.contains(['\n', '\t', '"', '\'', ' ', '\r']);

    if needs_quoting {
        let escaped = value
            .replace('\\', "\\\\")
            .replace('\n', "\\n")
            .replace('\t', "\\t")
            .replace('\r', "\\r")
            .replace('"', "\\\"");
        format!("\"{escaped}\"")
    } else {
        value.to_string()
    }
}

/// Atomic write: delegates to paths::atomic_write_io (preserves error detail).
fn atomic_write(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    crate::paths::atomic_write_io(path, content)
}

pub fn write_config_toml_path(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    atomic_write(path, content)?;
    lock_down_config_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn lock_down_config_permissions(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn lock_down_config_permissions(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
