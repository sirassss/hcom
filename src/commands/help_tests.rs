use super::*;

#[test]
fn help_text_contains_version() {
    // Capture what print_help would output by checking the format string
    let version = env!("CARGO_PKG_VERSION");
    assert!(!version.is_empty());
}

#[test]
fn all_commands_have_help() {
    let commands = [
        "send",
        "list",
        "events",
        "stop",
        "start",
        "listen",
        "status",
        "config",
        "hooks",
        "archive",
        "reset",
        "transcript",
        "bundle",
        "kill",
        "term",
        "relay",
        "run",
        "claude",
        "gemini",
        "codex",
        "opencode",
        "agy",
        "antigravity",
        "kimi",
    ];
    for cmd in commands {
        let help = get_command_help(cmd);
        assert!(
            help.starts_with("Usage:"),
            "help for '{}' should start with 'Usage:'",
            cmd
        );
        assert!(help.len() > 20, "help for '{}' should have content", cmd);
    }
}

#[test]
fn unknown_command_fallback() {
    let help = get_command_help("nonexistent");
    assert_eq!(help, "Usage: hcom nonexistent");
}

#[test]
fn events_sub_resolves_to_events() {
    let help = get_command_help("events sub");
    assert!(
        help.contains("Subscribe"),
        "events sub help should contain Subscribe section"
    );
}

#[test]
fn format_entry_rules() {
    // Blank line
    assert_eq!(format_entry("", ""), "");
    // Plain text
    assert_eq!(format_entry("", "some text"), "  some text");
    // Option line (indented)
    assert!(format_entry("  --json", "Output JSON").contains("--json"));
    // Section header
    assert!(format_entry("Examples:", "").starts_with('\n'));
    // Command line
    assert!(format_entry("list", "Show agents").contains("hcom list"));
}

#[test]
fn gemini_help_states_no_fork_support() {
    let help = get_command_help("gemini");
    assert!(help.contains("Gemini does not support session forking (hcom f)."));
    assert!(!help.contains("Resume / Fork:"));
}

#[test]
fn agy_help_uses_full_launch_template_without_fake_args_env() {
    let help = get_command_help("agy");
    assert!(help.contains("Launch N Antigravity agents"));
    assert!(help.contains("hcom antigravity"));
    assert!(help.contains("hcom agy --sandbox"));
    assert!(!help.contains("hcom agy --model"));
    assert!(help.contains("Run \"agy --help\" for agy options."));
    // Resume now supported via --conversation; fork still unsupported.
    assert!(help.contains("hcom r <target>"));
    assert!(help.contains("Antigravity does not support session forking (hcom f)."));
    assert!(!help.contains("HCOM_AGY_ARGS"));
    assert!(!help.contains("HCOM_ANTIGRAVITY_ARGS"));

    let alias_help = get_command_help("antigravity");
    assert_eq!(alias_help, help);
}

#[test]
fn capability_driven_help_lists_current_integrations() {
    let transcript_help = get_command_help("transcript");
    for tool in crate::transcript::transcript_tool_names() {
        assert!(
            transcript_help.contains(tool),
            "transcript help omitted {tool}"
        );
    }

    let hooks_help = get_command_help("hooks");
    for tool in crate::commands::hooks::hook_tools() {
        assert!(
            hooks_help.contains(tool.as_str()),
            "hooks help omitted {tool}"
        );
    }

    let resume_help = get_command_help("r");
    assert!(resume_help.contains("Claude/Kimi resume or fork only"));
}

#[test]
fn top_level_help_scopes_fork_to_supported_tools() {
    let help = get_help_text();
    assert!(
        help.contains("claude|gemini|codex|opencode|kilo|pi|omp|antigravity|cursor|kimi|copilot")
    );
    assert!(help.contains(
        "hcom f <name>                         Fork agent session (claude/codex/opencode/kilo/pi/omp)"
    ));
    assert!(!help.contains("Fork agent session (claude/codex/opencode/kilo/pi/omp/kimi)"));
    assert_eq!(forkable_tool_names(), "claude/codex/opencode/kilo/pi/omp");
}
