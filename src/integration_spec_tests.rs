use super::*;
use std::collections::HashSet;

#[test]
fn kimi_initial_prompt_is_explicitly_unsupported() {
    assert!(matches!(
        KIMI.launch.initial_prompt,
        InitialPromptShape::Unsupported { .. }
    ));
}

#[test]
fn every_tool_variant_has_a_spec() {
    for tool in [
        Tool::Claude,
        Tool::Gemini,
        Tool::Codex,
        Tool::OpenCode,
        Tool::Kilo,
        Tool::Antigravity,
        Tool::Cursor,
        Tool::Kimi,
        Tool::Copilot,
        Tool::Pi,
        Tool::Omp,
        Tool::Adhoc,
    ] {
        let spec = tool.spec();
        assert_eq!(spec.tool, tool, "spec.tool mismatch for {tool:?}");
    }
}

#[test]
fn spec_name_round_trips_through_from_str() {
    for spec in ALL {
        let parsed: Tool = spec.name.parse().expect("parses");
        assert_eq!(parsed, spec.tool);
    }
}

#[test]
fn hook_names_disjoint_among_primary_specs() {
    let mut seen: HashSet<&'static str> = HashSet::new();
    for spec in ALL {
        if spec.hooks.shared_hooks_with.is_some() {
            continue;
        }
        for name in spec.hooks.names {
            assert!(
                seen.insert(*name),
                "{} is owned by more than one routing spec",
                name
            );
        }
    }
}

#[test]
fn antigravity_hooks_match_gemini() {
    assert_eq!(ANTIGRAVITY.hooks.names, GEMINI.hooks.names);
    assert_eq!(ANTIGRAVITY.hooks.shared_hooks_with, Some(Tool::Gemini));
}

#[test]
fn kilo_hooks_match_opencode() {
    assert_eq!(KILO.hooks.names, OPENCODE.hooks.names);
    assert_eq!(KILO.hooks.shared_hooks_with, Some(Tool::OpenCode));
}

#[test]
fn adhoc_is_quiet() {
    assert!(ADHOC.hooks.names.is_empty());
    assert_eq!(ADHOC.hooks.invocation, HookInvocation::None);
    assert!(ADHOC.resume.is_none());
    assert!(!ADHOC.released);
}

#[test]
fn released_tools_matches_prior_constant() {
    let names = released_tool_names();
    assert!(names.contains(&"claude"));
    assert!(names.contains(&"gemini"));
    assert!(names.contains(&"codex"));
    assert!(names.contains(&"opencode"));
    assert!(names.contains(&"kilo"));
    assert!(names.contains(&"pi"));
    assert!(names.contains(&"antigravity"));
    assert!(names.contains(&"cursor"));
    assert!(names.contains(&"kimi"));
    assert!(names.contains(&"copilot"));
    assert!(names.contains(&"omp"));
    assert_eq!(names.len(), 11);
}

#[test]
fn released_background_matches_prior_constant() {
    assert_eq!(released_background_tool_names(), vec!["claude"]);
}

#[test]
fn every_tool_declares_a_nonzero_pty_delivery_timeout() {
    for spec in ALL {
        assert!(
            spec.pty.delivery_start_timeout_secs > 0,
            "{} must declare a PTY delivery timeout",
            spec.name
        );
    }
    assert_eq!(GEMINI.pty.delivery_start_timeout_secs, 60);
    assert_eq!(COPILOT.pty.delivery_start_timeout_secs, 60);
    assert_eq!(CLAUDE.pty.delivery_start_timeout_secs, 5);
}

#[test]
fn claude_uses_pty_default_false() {
    assert!(!CLAUDE.launch.uses_pty_default);
    for spec in ALL {
        if spec.tool == Tool::Claude || spec.tool == Tool::Adhoc {
            continue;
        }
        assert!(
            spec.launch.uses_pty_default,
            "{} should default to PTY",
            spec.name
        );
    }
}

#[test]
fn aliases_resolve_to_owning_spec() {
    for spec in ALL {
        for alias in spec.aliases {
            let parsed: Tool = alias.parse().expect("alias should parse");
            assert_eq!(
                parsed, spec.tool,
                "alias {} resolved to {:?}, expected {:?}",
                alias, parsed, spec.tool
            );
        }
    }
}

#[test]
fn aliases_are_globally_unique() {
    let mut seen: HashSet<&'static str> = HashSet::new();
    for spec in ALL {
        assert!(
            seen.insert(spec.name),
            "name {} is duplicated across specs",
            spec.name
        );
        for alias in spec.aliases {
            assert!(
                seen.insert(*alias),
                "alias {} collides with another tool name or alias",
                alias
            );
        }
    }
}

#[test]
fn released_specs_have_resume_and_cli_binary() {
    for spec in ALL {
        if !spec.released {
            continue;
        }
        assert!(
            !spec.cli_binary.is_empty(),
            "released tool {} must have a cli_binary",
            spec.name
        );
        assert!(
            spec.resume.is_some(),
            "released tool {} must define a resume spec (use None only for Adhoc)",
            spec.name
        );
    }
}

#[test]
fn max_launch_count_only_zero_for_unreleased() {
    for spec in ALL {
        if spec.released {
            assert!(
                spec.launch.max_launch_count > 0,
                "released tool {} must have max_launch_count > 0",
                spec.name
            );
        }
    }
}

#[test]
fn max_launch_count_matches_background_capability() {
    // Only claude supports headless launch today; if that ever changes,
    // released_background_tool_names() and max_launch_count budgets should
    // grow together — flag the assumption here.
    assert_eq!(released_background_tool_names(), vec!["claude"]);
    assert_eq!(CLAUDE.launch.max_launch_count, 100);
    for spec in ALL {
        if spec.tool != Tool::Claude && spec.released {
            assert_eq!(
                spec.launch.max_launch_count, 10,
                "{} should match the non-background cap of 10",
                spec.name
            );
        }
    }
}

// ── Released-tool drift gate ────────────────────────────────────────
//
// Every released tool must clear each gate below or document an explicit
// opt-out in its spec (e.g. Antigravity's `args_env: None` opts out of
// HCOM_*_ARGS config; Gemini/Antigravity's `resume.fork = None` opts out
// of `hcom f`). Adding a new released tool means filling these surfaces
// or pinning the opt-out here next to the existing carve-outs.

#[test]
fn drift_released_tools_resolve_via_launch_tool() {
    // Adding a released tool must wire it into LaunchTool::from_str so
    // `hcom <tool>` reaches the launcher. Each canonical name AND each
    // alias must parse and resolve back to the owning Tool variant.
    use crate::launcher::LaunchTool;
    for spec in ALL {
        if !spec.released {
            continue;
        }
        let lt = LaunchTool::from_str(spec.name)
            .unwrap_or_else(|e| panic!("LaunchTool::from_str({}) failed: {e}", spec.name));
        assert_eq!(
            lt.tool(),
            spec.tool,
            "{}: LaunchTool::tool() did not resolve to owning Tool",
            spec.name
        );
        assert_eq!(
            lt.cli_binary(),
            spec.cli_binary,
            "{}: LaunchTool::cli_binary() must match spec",
            spec.name
        );
        for alias in spec.aliases {
            let lt = LaunchTool::from_str(alias)
                .unwrap_or_else(|e| panic!("LaunchTool::from_str alias {alias} failed: {e}"));
            assert_eq!(
                lt.tool(),
                spec.tool,
                "alias {alias} did not resolve to owning Tool"
            );
        }
    }
}

#[test]
fn drift_released_tools_have_help_referencing_label() {
    // Every released tool — canonical name and each alias — must produce
    // launch help that starts with "Usage:" and references the spec's
    // human-readable label. Generated by commands::help::generate_tool_help
    // from the spec itself, so failure here means a spec field that
    // affects help rendering was renamed without updating the template.
    use crate::commands::help::get_command_help;
    for spec in ALL {
        if !spec.released {
            continue;
        }
        let help = get_command_help(spec.name);
        assert!(
            help.starts_with("Usage:"),
            "help for {} must start with 'Usage:'",
            spec.name
        );
        let label_marker = format!("Launch N {} agents", spec.label);
        assert!(
            help.contains(&label_marker),
            "help for {} must reference label '{}' (looked for '{}'):\n{}",
            spec.name,
            spec.label,
            label_marker,
            help
        );
        for alias in spec.aliases {
            let alias_help = get_command_help(alias);
            assert!(
                alias_help.starts_with("Usage:"),
                "alias help for {alias} must start with 'Usage:'"
            );
        }
    }
}

#[test]
fn drift_released_tools_have_hook_dispatch() {
    // Every released hook-bearing tool must round-trip through Tool's
    // hook-ops adapter: settings_path resolves to a non-empty path and
    // verify_hooks_installed() can be called without panicking.
    // Borrowed-hooks specs (Antigravity → Gemini) are checked via their
    // owning Tool — Antigravity has its own hook module but borrows the
    // hook command names.
    for spec in ALL {
        if !spec.released || spec.hooks.names.is_empty() {
            continue;
        }
        let path = spec.tool.hooks_settings_path();
        assert!(
            !path.is_empty(),
            "{}: Tool::hooks_settings_path() must not be empty for a hook-bearing released tool",
            spec.name
        );
        // verify is a read-only check; just confirm it doesn't panic.
        let _ = spec.tool.verify_hooks_installed(false);
    }
}

#[test]
fn drift_released_tools_args_env_documented_in_config() {
    // args_env opt-out is allowed (Antigravity is None today); when set,
    // the env-var name must point at a real HcomConfig field. The mapping
    // table lives in config.rs and is asserted in
    // `args_env_keys_match_integration_specs` — this gate just pins the
    // expectation that each released spec either sets args_env to a
    // non-empty string or explicitly opts out via None.
    for spec in ALL {
        if !spec.released {
            continue;
        }
        if let Some(env_var) = spec.launch.args_env {
            assert!(
                env_var.starts_with("HCOM_") && env_var.ends_with("_ARGS"),
                "{}: args_env '{}' must follow HCOM_*_ARGS naming",
                spec.name,
                env_var
            );
        }
    }
}

#[test]
fn drift_released_tools_with_args_env_merge_their_config_field() {
    use crate::commands::launch::merge_tool_args;
    use crate::config::HcomConfig;
    use crate::launcher::LaunchTool;

    for spec in ALL {
        let Some(args_env) = spec.launch.args_env else {
            continue;
        };
        if !spec.released {
            continue;
        }

        let field = args_env
            .strip_prefix("HCOM_")
            .expect("args env uses HCOM_ prefix")
            .to_ascii_lowercase();
        let mut config = HcomConfig::default();
        config
            .set_field(&field, "--model config-model")
            .unwrap_or_else(|e| panic!("{} config field {field}: {e}", spec.name));
        let launch_tool = LaunchTool::from_str(spec.name).unwrap();
        let merged = merge_tool_args(&launch_tool, &[], &config);

        assert!(
            merged.iter().any(|arg| arg == "config-model"),
            "{} must consume config field {field} declared by {args_env}",
            spec.name
        );
    }
}

#[test]
fn drift_released_tools_have_background_mode() {
    // Every released tool must declare a background mode other than
    // Unsupported. Unsupported is reserved for Adhoc, which is never
    // launched through `hcom [N] <tool>`.
    for spec in ALL {
        if spec.released {
            assert_ne!(
                spec.launch.background,
                BackgroundMode::Unsupported,
                "{}: released tool must declare a BackgroundMode other than Unsupported",
                spec.name
            );
        } else {
            assert_eq!(
                spec.launch.background,
                BackgroundMode::Unsupported,
                "{}: unreleased tool must declare BackgroundMode::Unsupported",
                spec.name
            );
        }
    }
}

#[test]
fn drift_native_print_implies_large_launch_budget() {
    // The 100-agent budget exists because NativePrint background is
    // truly detached (no terminal cost). HeadlessPty caps at 10 because
    // each instance still consumes a runner. If a new tool gains
    // NativePrint, bump max_launch_count to match Claude or update this
    // gate.
    for spec in ALL {
        if !spec.released {
            continue;
        }
        match spec.launch.background {
            BackgroundMode::NativePrint => {
                assert_eq!(
                    spec.launch.max_launch_count, 100,
                    "{}: NativePrint background expects a 100-agent budget",
                    spec.name
                );
            }
            BackgroundMode::HeadlessPty => {
                assert_eq!(
                    spec.launch.max_launch_count, 10,
                    "{}: HeadlessPty background expects a 10-agent budget",
                    spec.name
                );
            }
            BackgroundMode::Unsupported => {
                unreachable!("released tools handled by drift_released_tools_have_background_mode")
            }
        }
    }
}

#[test]
fn command_names_covers_released_tools() {
    use crate::commands::help::COMMAND_NAMES;
    for spec in ALL {
        if !spec.released {
            continue;
        }
        assert!(
            COMMAND_NAMES.contains(&spec.name),
            "COMMAND_NAMES missing released tool {}",
            spec.name
        );
        for alias in spec.aliases {
            assert!(
                COMMAND_NAMES.contains(alias),
                "COMMAND_NAMES missing alias {} for released tool {}",
                alias,
                spec.name
            );
        }
    }
}
