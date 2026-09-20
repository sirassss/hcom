//! Unified per-tool integration registry.
//!
//! One `IntegrationSpec` constant per [`Tool`](crate::tool::Tool) variant holds
//! the per-tool facts previously scattered across `tool.rs`, `delivery.rs`,
//! `commands/help.rs`, `hooks/family.rs`, `tui/render/agents.rs`, `launcher.rs`,
//! `commands/launch.rs`, and `commands/resume.rs`.
//!
//! The spec is the configuration plane, not "one file to add a tool" (although that would clearly be better if possible):
//! behavioral integration (hook handler bodies, binding, delivery injection,
//! preprocessing, plugins, arg parsers) still lives in dedicated modules.
//!
//! Not included on purpose:
//! - `HOOK_REGISTRY` (Claude-only; lives in `hooks/utils.rs` as its own
//!   registry — already a single source of truth, no drift across tools).
//! - Tool environment detection (owned by `shared::tool_detection`, including
//!   precedence and child-env clearing).
//! - Transcript parser implementations (owned by `transcript::TranscriptBackend`; canonical identity stays `Tool`).
//! - System-prompt env var keys (typed fields on `HcomConfig`; lookup goes
//!   through `config.rs::FIELD_TO_ENV`).

use crate::tool::Tool;

/// Help entry: `(usage, description)`. Mirrors `commands/help.rs::HelpEntry`.
pub type HelpEntry = (&'static str, &'static str);

/// How the external tool invokes hcom hook commands.
///
/// This is descriptive metadata for compatibility/spec auditing. Runtime hook
/// dispatch is driven by hook command names and per-tool handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookInvocation {
    /// JSON payload on stdin (Claude, Gemini, Codex, Antigravity).
    JsonStdin,
    /// argv-style subcommand (OpenCode, Kilo, Pi, OMP).
    Argv,
    /// No hooks (Adhoc).
    None,
}

#[derive(Debug, Clone, Copy)]
pub struct HooksSpec {
    /// Hook command names this tool answers to (e.g. `["poll", "post", …]`).
    pub names: &'static [&'static str],
    /// If set, this tool borrows another tool's hook command names. Routing
    /// should resolve those names to the owner, not the borrowing tool.
    ///
    /// Antigravity borrows Gemini hook names and is identified out-of-band by
    /// `ANTIGRAVITY_AGENT`, so `Tool::Antigravity.owns_hook("gemini-*")` must
    /// stay false even though this spec lists Gemini's hook names.
    pub shared_hooks_with: Option<Tool>,
    pub invocation: HookInvocation,
}

/// PTY delivery gate booleans. Mirrors fields on `delivery::ToolConfig`.
#[derive(Debug, Clone, Copy)]
pub struct GatesSpec {
    pub require_idle: bool,
    pub require_ready_prompt: bool,
    pub require_prompt_empty: bool,
    pub block_on_user_activity: bool,
    pub block_on_approval: bool,
    pub launch_requires_ready: bool,
    /// Treat the plugin's extension bind (a `kind='plugin'` notify endpoint) as
    /// launch readiness, in addition to the on-screen `ready_pattern`. For
    /// plugin-driven tools whose visible chrome is theme/preset configurable
    /// (OMP: status-line presets omit the pi glyph), the bind is the only
    /// rendering-independent proof the interactive TUI is up.
    pub launch_ready_on_plugin_bind: bool,
}

/// Tool background-launch capability.
///
/// Captures the spectrum currently expressed by `released_background: bool` and
/// the dispatch table in [`LaunchBackend::resolve`]. Used to:
///
/// - Drive `released_background_tool_names()` (filters for `NativePrint`).
/// - Document, in one place, how each tool behaves when `--headless` is set.
/// - Provide a typed gate for adding future tools: a new spec must pick a
///   variant rather than implicitly defaulting to "no background".
///
/// Variants:
/// - `Unsupported`: tool has no background launch path. Currently only Adhoc.
/// - `HeadlessPty`: background is routed through the PTY wrapper in a detached
///   runner; the live TUI keeps running there. gemini, codex, opencode, agy.
/// - `NativePrint`: background launches a detached process in the tool's own
///   print mode (Claude `-p --output-format stream-json --verbose`), opt-in via
///   an explicit `-p`/`--print` and kept alive across turns by hcom's stop-hook
///   loop. Default Claude `--headless` routes through `HeadlessPty` (live PTY
///   session) instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackgroundMode {
    Unsupported,
    HeadlessPty,
    NativePrint,
}

/// How `--hcom-prompt` becomes tool-side CLI args.
#[derive(Debug, Clone, Copy)]
pub enum InitialPromptShape {
    /// The tool cannot accept an interactive initial prompt at launch.
    Unsupported { reason: &'static str },
    /// `claude` — `--` then positional.
    DashDashPositional,
    /// `gemini`, `codex` — bare positional (interactive).
    Positional,
    /// `opencode --prompt <text>`, `agy --prompt-interactive <text>`.
    Flag(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub struct PtySpec {
    /// Maximum time to wait for the tool-specific ready pattern before
    /// starting delivery anyway. Every known integration declares this
    /// explicitly; ad-hoc commands use the Adhoc profile.
    pub delivery_start_timeout_secs: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct LaunchSpec {
    /// Env var holding default CLI args (e.g. `HCOM_CLAUDE_ARGS`).
    pub args_env: Option<&'static str>,
    /// Env var holding the tool's config directory (e.g. `CLAUDE_CONFIG_DIR`).
    pub config_dir_env: Option<&'static str>,
    pub initial_prompt: InitialPromptShape,
    /// PTY-by-default. `false` only for `claude` (which has a `claude-pty`
    /// alias for the PTY path).
    pub uses_pty_default: bool,
    /// Max agents per `hcom [N] <tool>` invocation. Claude gets a larger budget
    /// because of background bulk-launch (`-p` mode); others are capped at 10.
    pub max_launch_count: usize,
    /// Background-launch capability (see [`BackgroundMode`]).
    pub background: BackgroundMode,
}

/// Per-tool resume argument shape.
#[derive(Debug, Clone, Copy)]
pub enum ResumeArgs {
    /// `--resume <id>`, `--session <id>`, `--conversation <id>`.
    Flag(&'static str),
    /// `resume <id>` (Codex subcommand).
    Subcommand(&'static str),
}

/// Per-tool fork argument shape.
#[derive(Debug, Clone, Copy)]
pub enum ForkArgs {
    /// Resume + append flag (claude `--fork-session`, opencode `--fork`).
    AppendFlag(&'static str),
    /// Fork is a sibling subcommand (Codex `fork <id>`).
    Subcommand(&'static str),
}

#[derive(Debug, Clone, Copy)]
pub struct ResumeSpec {
    pub resume: ResumeArgs,
    pub fork: Option<ForkArgs>,
}

#[derive(Debug, Clone, Copy)]
pub struct HelpSpec {
    pub unique_examples: &'static [HelpEntry],
    pub extra_env: &'static [HelpEntry],
}

/// Tool-input field mappings used by `extract_tool_detail` for status display.
/// Each category lists the per-tool tool-call names that produce that kind of
/// activity (bash command, file write, subagent delegate).
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusDetailSpec {
    pub bash: &'static [&'static str],
    pub file: &'static [&'static str],
    pub delegate: &'static [&'static str],
}

/// Complete per-tool integration data.
pub struct IntegrationSpec {
    pub tool: Tool,
    pub name: &'static str,
    pub label: &'static str,
    /// CLI aliases beyond `name` (e.g. `["agy"]` for Antigravity).
    pub aliases: &'static [&'static str],
    /// Executable name on PATH. Usually matches `name`; differs for Antigravity
    /// (`agy`).
    pub cli_binary: &'static str,
    /// 4-char TUI prefix shown in multi-tool agent lists.
    pub tui_prefix: &'static str,
    /// Static icon used when agent status doesn't drive the glyph (Adhoc only).
    pub adhoc_icon: Option<&'static str>,
    /// True if this tool is in the public `RELEASED_TOOLS` set.
    pub released: bool,
    /// PTY ready-pattern bytes (empty for Adhoc).
    pub ready_pattern: &'static [u8],
    pub pty: PtySpec,
    /// Environment variables specific to this tool's instance state that
    /// will corrupt a same-tool child if leaked (session IDs, sandbox modes,
    /// config-directory pointers, process-role assignments, per-instance
    /// server URLs). Stripped before inheritance and `unset` in runner
    /// scripts. Must NOT include auth/API-key env vars or user config
    /// toggles (those forward). These are in ADDITION to TOOL_MARKER_VARS
    /// which already covers the broad-detection markers.
    ///
    /// Entries must meet ALL THREE criteria:
    /// (a) read as input on fresh start
    /// (b) instance-specific (session/thread/sandbox/path/role)
    /// (c) NOT re-set by the child's own launch
    ///
    /// This is a known-vars strip — NOT a completeness guarantee. Unknown
    /// instance-state vars on closed-source tools, or future vars on any
    /// tool, are a documented gap (same risk the PTY path already lives with
    /// today). Recoverable by adding the var when discovered.
    pub instance_state_env: &'static [&'static str],

    pub hooks: HooksSpec,
    pub gates: GatesSpec,
    pub launch: LaunchSpec,
    pub resume: Option<ResumeSpec>,
    pub help: HelpSpec,
    pub status_detail: StatusDetailSpec,
}

// ── Hook command name tables ────────────────────────────────────────────

const CLAUDE_HOOKS: &[&str] = &[
    "poll",
    "notify",
    "permission-request",
    "pre",
    "post",
    "post-failure",
    "permission-denied",
    "stop-failure",
    "sessionstart",
    "userpromptsubmit",
    "sessionend",
    "subagent-start",
    "subagent-stop",
];

const GEMINI_HOOKS: &[&str] = &[
    "gemini-sessionstart",
    "gemini-beforeagent",
    "gemini-afteragent",
    "gemini-beforetool",
    "gemini-aftertool",
    "gemini-notification",
    "gemini-sessionend",
];

const CODEX_HOOKS: &[&str] = &[
    "codex-sessionstart",
    "codex-userpromptsubmit",
    "codex-pretooluse",
    "codex-posttooluse",
    "codex-stop",
];

const OPENCODE_HOOKS: &[&str] = &[
    "opencode-start",
    "opencode-status",
    "opencode-read",
    "opencode-stop",
];

const PI_HOOKS: &[&str] = &[
    "pi-start",
    "pi-status",
    "pi-read",
    "pi-beforetool",
    "pi-stop",
];

const OMP_HOOKS: &[&str] = &[
    "omp-start",
    "omp-status",
    "omp-read",
    "omp-beforetool",
    "omp-stop",
];

const CURSOR_HOOKS: &[&str] = &[
    "cursor-sessionstart",
    "cursor-beforesubmitprompt",
    "cursor-pretooluse",
    "cursor-posttooluse",
    "cursor-stop",
    "cursor-sessionend",
];

const KIMI_HOOKS: &[&str] = &[
    "kimi-sessionstart",
    "kimi-userpromptsubmit",
    "kimi-pretooluse",
    "kimi-posttooluse",
    "kimi-permissionrequest",
    "kimi-permissionresult",
    "kimi-stop",
    "kimi-sessionend",
    "kimi-subagentstart",
    "kimi-subagentstop",
    "kimi-notification",
];

const COPILOT_HOOKS: &[&str] = &[
    "copilot-sessionstart",
    "copilot-userpromptsubmit",
    "copilot-pretooluse",
    "copilot-permissionrequest",
    "copilot-posttooluse",
    "copilot-posttoolusefailure",
    "copilot-notification",
    "copilot-agentstop",
    "copilot-subagentstart",
    "copilot-subagentstop",
    "copilot-sessionend",
];

// ── Help examples / extra-env tables ────────────────────────────────────

const CLAUDE_HELP_EXAMPLES: &[HelpEntry] = &[
    ("hcom 1 claude --agent <name>", ".claude/agents/<name>.md"),
    (
        "hcom claude --model sonnet|opus|haiku",
        "Use a specific model",
    ),
];
const CLAUDE_HELP_EXTRA_ENV: &[HelpEntry] = &[(
    "HCOM_SUBAGENT_TIMEOUT",
    "Seconds subagents keep-alive after task",
)];

const GEMINI_HELP_EXAMPLES: &[HelpEntry] = &[
    ("hcom N gemini --yolo", "Flags forwarded to gemini"),
    (
        "hcom gemini --model gemini-3.1-pro-preview|gemini-2.5-flash",
        "Use a specific model",
    ),
];
const GEMINI_HELP_EXTRA_ENV: &[HelpEntry] =
    &[("HCOM_GEMINI_SYSTEM_PROMPT", "System prompt (env var)")];

const CODEX_HELP_EXAMPLES: &[HelpEntry] = &[
    (
        "hcom codex --sandbox danger-full-access",
        "Flags forwarded to codex",
    ),
    (
        "hcom codex --model gpt-5.4|gpt-5.4-mini",
        "Use a specific model",
    ),
];
const CODEX_HELP_EXTRA_ENV: &[HelpEntry] = &[
    (
        "HCOM_CODEX_SYSTEM_PROMPT",
        "System prompt (env var or config)",
    ),
    (
        "HCOM_CODEX_SANDBOX_MODE",
        "workspace | untrusted | danger-full-access | none",
    ),
];

const OPENCODE_HELP_EXAMPLES: &[HelpEntry] = &[(
    "hcom opencode --model anthropic/claude-sonnet-4-6|openai/gpt-5.4",
    "Use a specific model",
)];

const KILO_HELP_EXAMPLES: &[HelpEntry] = &[(
    "hcom kilo --model kilo/kilo-auto/free",
    "Use Kilo's free auto model",
)];

const PI_HELP_EXAMPLES: &[HelpEntry] =
    &[("hcom pi --model claude-3-5-sonnet", "Use a specific model")];

const OMP_HELP_EXAMPLES: &[HelpEntry] =
    &[("hcom omp --model claude-3-5-sonnet", "Use a specific model")];

const AGY_HELP_EXAMPLES: &[HelpEntry] = &[
    ("hcom antigravity", "Long-form alias"),
    ("hcom agy --sandbox", "Flags forwarded to agy"),
    ("hcom r <name>", "Resume a stopped agy session"),
];

const CURSOR_HELP_EXAMPLES: &[HelpEntry] = &[
    ("hcom cursor-agent --model sonnet-4", "Use a specific model"),
    (
        "hcom cursor-agent --force",
        "Allow commands unless explicitly denied",
    ),
];

const KIMI_HELP_EXAMPLES: &[HelpEntry] = &[
    ("hcom kimi --model kimi-k2.6", "Use a specific model"),
    ("hcom kimi --yolo", "Bypass permission prompts"),
];

const COPILOT_HELP_EXAMPLES: &[HelpEntry] = &[
    (
        "hcom copilot --model claude-haiku-4.5",
        "Use a specific model",
    ),
    (
        "hcom copilot --allow-tool 'shell(hcom:*)'",
        "Flags forwarded to copilot",
    ),
];

// ── Per-tool integration constants ──────────────────────────────────────

pub static CLAUDE: IntegrationSpec = IntegrationSpec {
    tool: Tool::Claude,
    name: "claude",
    label: "Claude",
    aliases: &[],
    cli_binary: "claude",
    tui_prefix: "cla ",
    adhoc_icon: None,
    released: true,
    ready_pattern: b"? for shortcuts",
    pty: PtySpec {
        delivery_start_timeout_secs: 5,
    },
    instance_state_env: &[],
    hooks: HooksSpec {
        names: CLAUDE_HOOKS,
        shared_hooks_with: None,
        invocation: HookInvocation::JsonStdin,
    },
    // - require_ready_prompt=false: status bar hides in accept-edits mode.
    // - require_prompt_empty=true: VT100 dim detection separates placeholder
    //   (dim) from user input (not dim).
    gates: GatesSpec {
        require_idle: true,
        require_ready_prompt: false,
        require_prompt_empty: true,
        block_on_user_activity: true,
        block_on_approval: true,
        launch_requires_ready: false,
        launch_ready_on_plugin_bind: false,
    },
    launch: LaunchSpec {
        args_env: Some("HCOM_CLAUDE_ARGS"),
        config_dir_env: Some("CLAUDE_CONFIG_DIR"),
        initial_prompt: InitialPromptShape::DashDashPositional,
        uses_pty_default: false,
        max_launch_count: 100,
        background: BackgroundMode::NativePrint,
    },
    resume: Some(ResumeSpec {
        resume: ResumeArgs::Flag("--resume"),
        fork: Some(ForkArgs::AppendFlag("--fork-session")),
    }),
    help: HelpSpec {
        unique_examples: CLAUDE_HELP_EXAMPLES,
        extra_env: CLAUDE_HELP_EXTRA_ENV,
    },
    status_detail: StatusDetailSpec {
        bash: &["Bash"],
        file: &["Write", "Edit"],
        delegate: &["Task", "Agent"],
    },
};

pub static GEMINI: IntegrationSpec = IntegrationSpec {
    tool: Tool::Gemini,
    name: "gemini",
    label: "Gemini",
    aliases: &[],
    cli_binary: "gemini",
    tui_prefix: "gem ",
    adhoc_icon: None,
    released: true,
    ready_pattern: b"Type your message",
    pty: PtySpec {
        delivery_start_timeout_secs: 60,
    },
    instance_state_env: &["GEMINI_PTY_INFO"],
    hooks: HooksSpec {
        names: GEMINI_HOOKS,
        shared_hooks_with: None,
        invocation: HookInvocation::JsonStdin,
    },
    // require_ready_prompt=true: "Type your message" placeholder disappears
    // instantly when user types, so visibility implies an empty prompt.
    gates: GatesSpec {
        require_idle: true,
        require_ready_prompt: true,
        require_prompt_empty: false,
        block_on_user_activity: true,
        block_on_approval: true,
        launch_requires_ready: true,
        launch_ready_on_plugin_bind: false,
    },
    launch: LaunchSpec {
        args_env: Some("HCOM_GEMINI_ARGS"),
        config_dir_env: Some("GEMINI_CLI_HOME"),
        initial_prompt: InitialPromptShape::Positional,
        uses_pty_default: true,
        max_launch_count: 10,
        background: BackgroundMode::HeadlessPty,
    },
    resume: Some(ResumeSpec {
        resume: ResumeArgs::Flag("--resume"),
        fork: None,
    }),
    help: HelpSpec {
        unique_examples: GEMINI_HELP_EXAMPLES,
        extra_env: GEMINI_HELP_EXTRA_ENV,
    },
    status_detail: StatusDetailSpec {
        bash: &["run_shell_command"],
        file: &["write_file", "replace"],
        delegate: &["delegate_to_agent"],
    },
};

pub static CODEX: IntegrationSpec = IntegrationSpec {
    tool: Tool::Codex,
    name: "codex",
    label: "Codex",
    aliases: &[],
    cli_binary: "codex",
    tui_prefix: "cod ",
    adhoc_icon: None,
    released: true,
    ready_pattern: "\u{203A} ".as_bytes(),
    pty: PtySpec {
        delivery_start_timeout_secs: 5,
    },
    instance_state_env: &["CODEX_EXEC_SERVER_URL"],
    hooks: HooksSpec {
        names: CODEX_HOOKS,
        shared_hooks_with: None,
        invocation: HookInvocation::JsonStdin,
    },
    // - require_ready_prompt=false: "? for shortcuts" hides in narrow terminals.
    // - require_prompt_empty=true: VT100 dim detection on the `›` prompt char.
    gates: GatesSpec {
        require_idle: true,
        require_ready_prompt: false,
        require_prompt_empty: true,
        block_on_user_activity: true,
        block_on_approval: true,
        launch_requires_ready: false,
        launch_ready_on_plugin_bind: false,
    },
    launch: LaunchSpec {
        args_env: Some("HCOM_CODEX_ARGS"),
        config_dir_env: Some("CODEX_HOME"),
        initial_prompt: InitialPromptShape::Positional,
        uses_pty_default: true,
        max_launch_count: 10,
        background: BackgroundMode::HeadlessPty,
    },
    resume: Some(ResumeSpec {
        resume: ResumeArgs::Subcommand("resume"),
        fork: Some(ForkArgs::Subcommand("fork")),
    }),
    help: HelpSpec {
        unique_examples: CODEX_HELP_EXAMPLES,
        extra_env: CODEX_HELP_EXTRA_ENV,
    },
    status_detail: StatusDetailSpec {
        bash: &["Bash", "execute_command", "shell", "shell_command"],
        file: &["apply_patch"],
        delegate: &[],
    },
};

pub static OPENCODE: IntegrationSpec = IntegrationSpec {
    tool: Tool::OpenCode,
    name: "opencode",
    label: "OpenCode",
    aliases: &[],
    cli_binary: "opencode",
    tui_prefix: "opc ",
    adhoc_icon: None,
    released: true,
    ready_pattern: b"ctrl+p commands",
    pty: PtySpec {
        delivery_start_timeout_secs: 5,
    },
    instance_state_env: &["OPENCODE_RUN_ID", "OPENCODE_PROCESS_ROLE"],
    hooks: HooksSpec {
        names: OPENCODE_HOOKS,
        shared_hooks_with: None,
        invocation: HookInvocation::Argv,
    },
    // Runtime gates off — the TypeScript plugin owns delivery after bootstrap.
    // launch_requires_ready=true so PTY bootstrap inject lands on a usable TUI.
    gates: GatesSpec {
        require_idle: false,
        require_ready_prompt: false,
        require_prompt_empty: false,
        block_on_user_activity: false,
        block_on_approval: false,
        launch_requires_ready: true,
        launch_ready_on_plugin_bind: false,
    },
    launch: LaunchSpec {
        args_env: Some("HCOM_OPENCODE_ARGS"),
        // OPENCODE_CONFIG_DIR is read by the plugin install/verify/remove path
        // in hooks/opencode.rs; surface it for the launch diagnostic dump.
        // This does NOT enable launcher config-isolation: isolated_tool_config_dir
        // returns None for OpenCode (it isolates via OPENCODE_RUN_ID/_PROCESS_ROLE),
        // so the auto-isolation arm in launcher.rs stays a no-op for this tool.
        config_dir_env: Some("OPENCODE_CONFIG_DIR"),
        initial_prompt: InitialPromptShape::Flag("--prompt"),
        uses_pty_default: true,
        max_launch_count: 10,
        background: BackgroundMode::HeadlessPty,
    },
    resume: Some(ResumeSpec {
        resume: ResumeArgs::Flag("--session"),
        fork: Some(ForkArgs::AppendFlag("--fork")),
    }),
    help: HelpSpec {
        unique_examples: OPENCODE_HELP_EXAMPLES,
        extra_env: &[],
    },
    status_detail: StatusDetailSpec {
        bash: &[],
        file: &[],
        delegate: &[],
    },
};

pub static KILO: IntegrationSpec = IntegrationSpec {
    tool: Tool::Kilo,
    name: "kilo",
    label: "Kilo Code",
    aliases: &["kilocode"],
    cli_binary: "kilo",
    tui_prefix: "kil ",
    adhoc_icon: None,
    released: true,
    ready_pattern: b"ctrl+p commands",
    // Kilo namespaces OpenCode's run/role state under its own vars (see
    // kilocode packages/core/src/util/opencode-process.ts: KILO_RUN_ID /
    // KILO_PROCESS_ROLE are `??=`-assigned, so an inherited value is reused
    // rather than regenerated → a leaked parent value collides a same-tool
    // child, exactly like OpenCode's OPENCODE_RUN_ID/OPENCODE_PROCESS_ROLE).
    // KILO_DB is the on-disk session store; all three are instance-state.
    pty: PtySpec {
        delivery_start_timeout_secs: 5,
    },
    instance_state_env: &["KILO_DB", "KILO_RUN_ID", "KILO_PROCESS_ROLE"],
    hooks: HooksSpec {
        names: OPENCODE_HOOKS,
        shared_hooks_with: Some(Tool::OpenCode),
        invocation: HookInvocation::Argv,
    },
    // Kilo is an OpenCode-family variant: its shared TypeScript plugin owns
    // delivery after the first PTY bootstrap turn.
    gates: GatesSpec {
        require_idle: false,
        require_ready_prompt: false,
        require_prompt_empty: false,
        block_on_user_activity: false,
        block_on_approval: false,
        launch_requires_ready: true,
        launch_ready_on_plugin_bind: false,
    },
    launch: LaunchSpec {
        args_env: Some("HCOM_KILO_ARGS"),
        config_dir_env: Some("KILO_CONFIG_DIR"),
        initial_prompt: InitialPromptShape::Flag("--prompt"),
        uses_pty_default: true,
        max_launch_count: 10,
        background: BackgroundMode::HeadlessPty,
    },
    resume: Some(ResumeSpec {
        resume: ResumeArgs::Flag("--session"),
        fork: Some(ForkArgs::AppendFlag("--fork")),
    }),
    help: HelpSpec {
        unique_examples: KILO_HELP_EXAMPLES,
        extra_env: &[],
    },
    status_detail: StatusDetailSpec {
        bash: &[],
        file: &[],
        delegate: &[],
    },
};

pub static ANTIGRAVITY: IntegrationSpec = IntegrationSpec {
    tool: Tool::Antigravity,
    name: "antigravity",
    label: "Antigravity",
    aliases: &["agy"],
    cli_binary: "agy",
    tui_prefix: "agy ",
    adhoc_icon: None,
    released: true,
    // Claude's pattern was copied here; agy never prints it. Its status bar
    // renders "Ctx <pct>% (<used>/<total>)" in every frame once the TUI is up.
    ready_pattern: b"Ctx ",
    pty: PtySpec {
        delivery_start_timeout_secs: 5,
    },
    instance_state_env: &["ANTIGRAVITY_EXECUTABLE_DATA_DIR", "GEMINI_PTY_INFO"],
    hooks: HooksSpec {
        names: GEMINI_HOOKS,
        // Antigravity reuses Gemini hook names; routing resolves those names
        // to Gemini. Identity is established via ANTIGRAVITY_AGENT.
        shared_hooks_with: Some(Tool::Gemini),
        invocation: HookInvocation::JsonStdin,
    },
    gates: GatesSpec {
        require_idle: true,
        require_ready_prompt: true,
        require_prompt_empty: true,
        block_on_user_activity: false,
        block_on_approval: true,
        launch_requires_ready: true,
        launch_ready_on_plugin_bind: false,
    },
    launch: LaunchSpec {
        args_env: None,
        // Antigravity shares Gemini's config tree.
        config_dir_env: Some("GEMINI_CLI_HOME"),
        initial_prompt: InitialPromptShape::Flag("--prompt-interactive"),
        uses_pty_default: true,
        max_launch_count: 10,
        background: BackgroundMode::HeadlessPty,
    },
    resume: Some(ResumeSpec {
        resume: ResumeArgs::Flag("--conversation"),
        fork: None,
    }),
    help: HelpSpec {
        // Help lookup uses "agy" externally; the agy unique examples document
        // the long-form alias as a forwarded flag.
        unique_examples: AGY_HELP_EXAMPLES,
        extra_env: &[],
    },
    status_detail: StatusDetailSpec {
        bash: &["run_command"],
        file: &[
            "write_to_file",
            "replace_file_content",
            "multi_replace_file_content",
        ],
        delegate: &["invoke_subagent"],
    },
};

pub static CURSOR: IntegrationSpec = IntegrationSpec {
    tool: Tool::Cursor,
    name: "cursor",
    label: "Cursor",
    aliases: &["cursor-agent"],
    cli_binary: "cursor-agent",
    tui_prefix: "cur ",
    adhoc_icon: None,
    released: true,
    // Cursor's input placeholder is styled rather than a stable ASCII footer.
    // Prompt-empty detection is the readiness signal for the MVP.
    ready_pattern: b"",
    // Closed-source; unknown instance-state vars are a documented gap.
    pty: PtySpec {
        delivery_start_timeout_secs: 5,
    },
    instance_state_env: &[],
    hooks: HooksSpec {
        names: CURSOR_HOOKS,
        shared_hooks_with: None,
        invocation: HookInvocation::JsonStdin,
    },
    gates: GatesSpec {
        require_idle: true,
        require_ready_prompt: false,
        require_prompt_empty: true,
        block_on_user_activity: true,
        block_on_approval: true,
        launch_requires_ready: true,
        launch_ready_on_plugin_bind: false,
    },
    launch: LaunchSpec {
        args_env: Some("HCOM_CURSOR_ARGS"),
        config_dir_env: Some("CURSOR_CONFIG_DIR"),
        initial_prompt: InitialPromptShape::Positional,
        uses_pty_default: true,
        max_launch_count: 10,
        background: BackgroundMode::HeadlessPty,
    },
    resume: Some(ResumeSpec {
        resume: ResumeArgs::Flag("--resume"),
        // O2: cursor-agent has no native fork/branch primitive (only `--resume`
        // / `--continue` / `create-chat`). Leave `fork: None` like gemini/agy;
        // simulating fork via resume+create-chat is deferred (not needed for
        // parity).
        fork: None,
    }),
    help: HelpSpec {
        unique_examples: CURSOR_HELP_EXAMPLES,
        extra_env: &[],
    },
    // Empirically verified against real cursor transcripts: cursor's edit tool
    // is `StrReplace` (not `Edit`, which never appears) and its file/edit tools
    // key the path off `path` (not `file_path` — see extract_tool_detail). Shell
    // also has the `run_terminal_cmd` variant; delegates are `Task`/`Subagent`.
    status_detail: StatusDetailSpec {
        bash: &["Shell", "run_terminal_cmd"],
        file: &["Write", "StrReplace"],
        delegate: &["Task", "Subagent"],
    },
};

pub static KIMI: IntegrationSpec = IntegrationSpec {
    tool: Tool::Kimi,
    name: "kimi",
    label: "Kimi",
    aliases: &[],
    cli_binary: "kimi",
    tui_prefix: "kim ",
    adhoc_icon: None,
    released: true,
    ready_pattern: b"> ",
    pty: PtySpec {
        delivery_start_timeout_secs: 5,
    },
    instance_state_env: &[],
    hooks: HooksSpec {
        names: KIMI_HOOKS,
        shared_hooks_with: None,
        invocation: HookInvocation::JsonStdin,
    },
    gates: GatesSpec {
        require_idle: true,
        require_ready_prompt: false,
        require_prompt_empty: true,
        block_on_user_activity: true,
        block_on_approval: true,
        launch_requires_ready: false,
        launch_ready_on_plugin_bind: false,
    },
    launch: LaunchSpec {
        args_env: Some("HCOM_KIMI_ARGS"),
        // Kimi's data root (config + sessions + credentials) is overridden via
        // KIMI_CODE_HOME; it does not honor a separate config-dir variable.
        config_dir_env: Some("KIMI_CODE_HOME"),
        initial_prompt: InitialPromptShape::Unsupported {
            reason: "kimi does not support an initial prompt at launch. Launch `hcom kimi` without a prompt, then send the task with `hcom send @<name> -- \"…\"`.",
        },
        uses_pty_default: true,
        max_launch_count: 10,
        background: BackgroundMode::HeadlessPty,
    },
    resume: Some(ResumeSpec {
        // Use --session as it is the documented flag in kimi --help.
        resume: ResumeArgs::Flag("--session"),
        // Kimi has no CLI fork primitive — `/fork` is an interactive slash
        // command only (`kimi --fork` errors "unknown option"). Like
        // gemini/cursor/agy, leave fork unsupported.
        fork: None,
    }),
    help: HelpSpec {
        unique_examples: KIMI_HELP_EXAMPLES,
        // No HCOM_KIMI_SYSTEM_PROMPT: kimi has no system-prompt config field or
        // injection path (see hooks/kimi.rs — it's a future kimi feature), so
        // advertising it here was a ghost.
        extra_env: &[],
    },
    // Tool names verified against kimi-code 0.9.0 built-in tools
    // (docs/reference/tools.md): shell is `Bash`, file writes are `Write`/`Edit`,
    // and the subagent tool is `Agent`.
    status_detail: StatusDetailSpec {
        bash: &["Bash"],
        file: &["Write", "Edit"],
        delegate: &["Agent"],
    },
};

pub static PI: IntegrationSpec = IntegrationSpec {
    tool: Tool::Pi,
    name: "pi",
    label: "Pi",
    aliases: &["pi-agent"],
    cli_binary: "pi",
    tui_prefix: "pi  ",
    adhoc_icon: None,
    released: true,
    ready_pattern: b"/ commands",
    pty: PtySpec {
        delivery_start_timeout_secs: 5,
    },
    instance_state_env: &[],
    hooks: HooksSpec {
        names: PI_HOOKS,
        shared_hooks_with: None,
        invocation: HookInvocation::Argv,
    },
    // Runtime gates off: the Pi extension owns delivery after bootstrap.
    gates: GatesSpec {
        require_idle: false,
        require_ready_prompt: false,
        require_prompt_empty: false,
        block_on_user_activity: false,
        block_on_approval: true,
        launch_requires_ready: true,
        launch_ready_on_plugin_bind: false,
    },
    launch: LaunchSpec {
        args_env: Some("HCOM_PI_ARGS"),
        config_dir_env: Some("PI_CODING_AGENT_DIR"),
        initial_prompt: InitialPromptShape::Positional,
        uses_pty_default: true,
        max_launch_count: 10,
        background: BackgroundMode::HeadlessPty,
    },
    resume: Some(ResumeSpec {
        resume: ResumeArgs::Flag("--session"),
        // Pi's `--fork <id>` takes the session id as its value and is mutually
        // exclusive with `--session` ("--fork cannot be combined with
        // --session"). Use Subcommand so build_resume_args emits
        // `["--fork", <id>]` (replacing `--session`), not `["--session", <id>,
        // "--fork"]` which pi rejects.
        fork: Some(ForkArgs::Subcommand("--fork")),
    }),
    help: HelpSpec {
        unique_examples: PI_HELP_EXAMPLES,
        extra_env: &[],
    },
    // `file` lists mutating ops only (like every other tool); Pi's `read` is a
    // read-only op and was producing a path in the status bar on plain reads.
    status_detail: StatusDetailSpec {
        bash: &["bash"],
        file: &["edit", "write"],
        delegate: &[],
    },
};

pub static OMP: IntegrationSpec = IntegrationSpec {
    tool: Tool::Omp,
    name: "omp",
    label: "Oh My Pi",
    aliases: &["omp-agent"],
    cli_binary: "omp",
    tui_prefix: "omp ",
    adhoc_icon: None,
    released: true,
    // No usable on-screen readiness marker. Pi's `/ commands` comes from its
    // startup-instructions header; OMP forked that away (WelcomeComponent never
    // renders it — the only `/ commands` in OMP is the first-run theme wizard).
    // Every other candidate lives in the status line, which is preset- and
    // theme-configurable: the minimal/compact/ascii/custom presets omit the pi
    // segment entirely and `icon.pi` is theme-selectable, so anchoring on `π `
    // still falsely blocks established configs. Readiness is instead proven by
    // the hcom extension's bind (`launch_ready_on_plugin_bind`), which is
    // rendering-independent. Empty pattern => is_ready() is always true, so it
    // never gates launch on scraped chrome; the plugin bind is authoritative.
    ready_pattern: b"",
    pty: PtySpec {
        delivery_start_timeout_secs: 5,
    },
    instance_state_env: &[],
    hooks: HooksSpec {
        names: OMP_HOOKS,
        shared_hooks_with: None,
        invocation: HookInvocation::Argv,
    },
    gates: GatesSpec {
        require_idle: false,
        require_ready_prompt: false,
        require_prompt_empty: false,
        block_on_user_activity: false,
        block_on_approval: true,
        launch_requires_ready: true,
        // OMP has no reliable on-screen ready marker (all candidates live in the
        // preset/theme-configurable status line), so launch readiness is proven
        // by the hcom extension's bind (kind='plugin' notify endpoint) instead —
        // rendering-independent, and a dead extension correctly fails to bind and
        // blocks. `ready_pattern` is empty; this is the authoritative signal.
        launch_ready_on_plugin_bind: true,
    },
    launch: LaunchSpec {
        args_env: Some("HCOM_OMP_ARGS"),
        config_dir_env: Some("PI_CODING_AGENT_DIR"),
        initial_prompt: InitialPromptShape::Positional,
        uses_pty_default: true,
        max_launch_count: 10,
        background: BackgroundMode::HeadlessPty,
    },
    resume: Some(ResumeSpec {
        resume: ResumeArgs::Flag("--resume"),
        // Top-level `omp --help` omits `--fork`, but the flag is real: it has
        // existed and been documented in session-operation docs since v15.1.9.
        // `omp --fork <id>` routes through `SessionManager.forkFrom(...)` (takes
        // the session id/path as its value) and is mutually exclusive with
        // session persistence (`omp --fork x --no-session` -> "--fork requires
        // session persistence"). Same shape as Pi: Subcommand emits
        // `["--fork", <id>]` (replacing `--resume`), not `["--resume", <id>,
        // "--fork"]`. Must not degrade `hcom f` into a plain `--resume`.
        fork: Some(ForkArgs::Subcommand("--fork")),
    }),
    help: HelpSpec {
        unique_examples: OMP_HELP_EXAMPLES,
        extra_env: &[],
    },
    status_detail: StatusDetailSpec {
        bash: &["bash"],
        file: &["edit", "write"],
        delegate: &[],
    },
};

pub static COPILOT: IntegrationSpec = IntegrationSpec {
    tool: Tool::Copilot,
    name: "copilot",
    label: "Copilot",
    aliases: &[],
    cli_binary: "copilot",
    tui_prefix: "cop ",
    adhoc_icon: None,
    released: true,
    // Copilot fires SessionStart twice: once at boot and again after it loads
    // hooks/instructions. Gate on "/ commands" footer text so delivery doesn't
    // inject during the loading window.
    ready_pattern: b"/ commands",
    // Closed-source; unknown instance-state vars are a documented gap.
    pty: PtySpec {
        delivery_start_timeout_secs: 60,
    },
    instance_state_env: &[],
    hooks: HooksSpec {
        names: COPILOT_HOOKS,
        shared_hooks_with: None,
        invocation: HookInvocation::JsonStdin,
    },
    gates: GatesSpec {
        require_idle: true,
        require_ready_prompt: true,
        require_prompt_empty: true,
        block_on_user_activity: true,
        block_on_approval: true,
        launch_requires_ready: true,
        launch_ready_on_plugin_bind: false,
    },
    launch: LaunchSpec {
        args_env: Some("HCOM_COPILOT_ARGS"),
        config_dir_env: Some("COPILOT_HOME"),
        initial_prompt: InitialPromptShape::Flag("-i"),
        uses_pty_default: true,
        max_launch_count: 10,
        background: BackgroundMode::HeadlessPty,
    },
    resume: Some(ResumeSpec {
        resume: ResumeArgs::Flag("--resume"),
        fork: None,
    }),
    help: HelpSpec {
        unique_examples: COPILOT_HELP_EXAMPLES,
        extra_env: &[],
    },
    status_detail: StatusDetailSpec {
        bash: &["bash", "powershell"],
        file: &["create", "edit", "apply_patch"],
        delegate: &["task"],
    },
};

pub static ADHOC: IntegrationSpec = IntegrationSpec {
    tool: Tool::Adhoc,
    name: "adhoc",
    label: "Adhoc",
    aliases: &[],
    cli_binary: "",
    tui_prefix: "ah  ",
    adhoc_icon: Some("\u{25e6}"), // ◦ neutral dot
    released: false,
    ready_pattern: b"",
    pty: PtySpec {
        delivery_start_timeout_secs: 60,
    },
    instance_state_env: &[],
    hooks: HooksSpec {
        names: &[],
        shared_hooks_with: None,
        invocation: HookInvocation::None,
    },
    // Adhoc delivery is manual via `hcom listen` / hookless CLI unread, so
    // these conservative gates are documented but mostly unused.
    gates: GatesSpec {
        require_idle: true,
        require_ready_prompt: false,
        require_prompt_empty: true,
        block_on_user_activity: true,
        block_on_approval: true,
        launch_requires_ready: false,
        launch_ready_on_plugin_bind: false,
    },
    launch: LaunchSpec {
        args_env: None,
        config_dir_env: None,
        initial_prompt: InitialPromptShape::Positional,
        uses_pty_default: false,
        max_launch_count: 0,
        background: BackgroundMode::Unsupported,
    },
    resume: None,
    help: HelpSpec {
        unique_examples: &[],
        extra_env: &[],
    },
    status_detail: StatusDetailSpec {
        bash: &[],
        file: &[],
        delegate: &[],
    },
};

/// All specs in canonical order. `Tool::spec` indexes into this implicitly via
/// match — exposed for iteration (released-list helpers, hook-name routing).
pub static ALL: &[&IntegrationSpec] = &[
    &CLAUDE,
    &GEMINI,
    &CODEX,
    &OPENCODE,
    &KILO,
    &PI,
    &OMP,
    &ANTIGRAVITY,
    &CURSOR,
    &KIMI,
    &COPILOT,
    &ADHOC,
];

impl Tool {
    /// Return this tool's integration spec.
    pub fn spec(self) -> &'static IntegrationSpec {
        match self {
            Tool::Claude => &CLAUDE,
            Tool::Gemini => &GEMINI,
            Tool::Codex => &CODEX,
            Tool::OpenCode => &OPENCODE,
            Tool::Kilo => &KILO,
            Tool::Omp => &OMP,
            Tool::Pi => &PI,
            Tool::Antigravity => &ANTIGRAVITY,
            Tool::Cursor => &CURSOR,
            Tool::Kimi => &KIMI,
            Tool::Copilot => &COPILOT,
            Tool::Adhoc => &ADHOC,
        }
    }
}

/// Tool names in the public `RELEASED_TOOLS` set, computed from specs.
pub fn released_tool_names() -> Vec<&'static str> {
    ALL.iter().filter(|s| s.released).map(|s| s.name).collect()
}

/// Tool names supporting background/headless launch via the tool's own
/// detached print mode (currently only Claude `-p`). PTY-wrapped headless
/// (`HeadlessPty`) is universal across released tools and not reported here —
/// this set drives the larger `max_launch_count` budget given to true detached
/// bulk launches.
pub fn released_background_tool_names() -> Vec<&'static str> {
    ALL.iter()
        .filter(|s| s.released && matches!(s.launch.background, BackgroundMode::NativePrint))
        .map(|s| s.name)
        .collect()
}

#[cfg(test)]
#[path = "integration_spec_tests.rs"]
mod tests;
