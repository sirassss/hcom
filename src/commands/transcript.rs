//! `hcom transcript` command — view and search agent conversation transcripts.
//!
//!
//! Supports:
//! - View transcript: `hcom transcript @instance [N | N-M] [--full] [--detailed] [--json] [--last N]`
//! - Timeline: `hcom transcript timeline [--last N] [--full] [--json]`
//! - Search: `hcom transcript search "pattern" [--live] [--all] [--limit N] [--agent TYPE]`

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::db::HcomDb;
use crate::shared::CommandContext;
use crate::tool::Tool;
use crate::transcript::{self, Exchange, ReadOptions, format_exchanges, summarize_action};

fn run_search_tool(program: &str, args: &[&str]) -> Result<Option<std::process::Output>, String> {
    match std::process::Command::new(program).args(args).output() {
        Ok(output) if output.status.success() => Ok(Some(output)),
        Ok(output) if output.status.code() == Some(1) => Ok(None),
        Ok(output) => {
            let detail = String::from_utf8_lossy(&output.stderr);
            Err(format!(
                "{program} failed{}",
                if detail.trim().is_empty() {
                    format!(" with {}", output.status)
                } else {
                    format!(": {}", detail.trim())
                }
            ))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(format!(
            "required search tool `{program}` was not found on PATH"
        )),
        Err(err) => Err(format!("could not run `{program}`: {err}")),
    }
}

/// Parsed arguments for `hcom transcript`.
#[derive(clap::Parser, Debug)]
#[command(name = "transcript", about = "View and search transcripts")]
pub struct TranscriptArgs {
    /// Subcommand (search, timeline) or view mode
    #[command(subcommand)]
    pub subcmd: Option<TranscriptSubcmd>,

    /// Target instance name (with or without @)
    pub name: Option<String>,
    /// Exchange range (e.g., "5" or "5-10")
    pub range_positional: Option<String>,

    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Full output (no streamlining)
    #[arg(long)]
    pub full: bool,
    /// Show tool inputs/outputs, file edits, and errors
    #[arg(long)]
    pub detailed: bool,
    /// Last N exchanges
    #[arg(long)]
    pub last: Option<usize>,
    /// Exchange range (flag form)
    #[arg(long = "range")]
    pub range_flag: Option<String>,
}

#[derive(clap::Subcommand, Debug)]
pub enum TranscriptSubcmd {
    /// Search transcripts for a pattern
    Search(TranscriptSearchArgs),
    /// Show timeline of all agents' recent activity
    Timeline(TranscriptTimelineArgs),
}

/// Args for `hcom transcript search`.
#[derive(clap::Args, Debug)]
pub struct TranscriptSearchArgs {
    /// Search pattern (regex)
    pub pattern: String,
    /// Live-watch mode
    #[arg(long)]
    pub live: bool,
    /// Search all transcripts on disk (not just tracked instances)
    #[arg(long)]
    pub all: bool,
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Exclude own transcript from search results
    #[arg(long)]
    pub exclude_self: bool,
    /// Max results (default: 20)
    #[arg(long, default_value = "20")]
    pub limit: usize,
    /// Filter by exact agent type (canonical name or declared alias)
    #[arg(long)]
    pub agent: Option<String>,
}

/// Args for `hcom transcript timeline`.
#[derive(clap::Args, Debug)]
pub struct TranscriptTimelineArgs {
    /// JSON output
    #[arg(long)]
    pub json: bool,
    /// Full output
    #[arg(long)]
    pub full: bool,
    /// Detailed output
    #[arg(long)]
    pub detailed: bool,
    /// Last N exchanges per agent
    #[arg(long)]
    pub last: Option<usize>,
}

/// Truncate a string to at most `max` bytes at a valid UTF-8 char boundary.
fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Snippet width (bytes) for transcript search matches, centered on the hit.
const SEARCH_SNIPPET_WIDTH: usize = 160;

/// Build a snippet of ~`width` bytes centered on a match.
///
/// Transcript lines are whole JSON message objects (thousands of bytes), so a
/// start-anchored truncation only ever shows leading metadata (`parentUuid`…),
/// never the match. `col` is ripgrep's 1-based byte column of the match start;
/// we window around it so the matched text is actually visible, with `…`
/// markers when content is elided on either side. All slice points are snapped
/// to UTF-8 char boundaries.
fn centered_snippet(line_text: &str, col: usize, width: usize) -> String {
    if line_text.len() <= width {
        return line_text.to_string();
    }
    let match_start = col.saturating_sub(1).min(line_text.len());
    let half = width / 2;
    let mut start = match_start.saturating_sub(half);
    while start > 0 && !line_text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (start + width).min(line_text.len());
    while end < line_text.len() && !line_text.is_char_boundary(end) {
        end += 1;
    }
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.push_str(line_text[start..end].trim());
    if end < line_text.len() {
        out.push('…');
    }
    out
}

/// Parse one search-tool output line into `(line_number, centered_snippet)`.
///
/// `has_column` is true for ripgrep run with `--column` (`LINE:COL:TEXT`) and
/// false for the `grep` fallback (`LINE:TEXT`), where we locate the pattern
/// literally to center on it and fall back to the line start otherwise.
fn parse_match_line(raw: &str, has_column: bool, pattern: &str) -> (usize, String) {
    let Some((line_str, rest)) = raw.split_once(':') else {
        return (0, centered_snippet(raw, 1, SEARCH_SNIPPET_WIDTH));
    };
    let line_num = line_str.parse::<usize>().unwrap_or(0);

    if has_column
        && let Some((col_str, text)) = rest.split_once(':')
        && let Ok(col) = col_str.parse::<usize>()
    {
        return (line_num, centered_snippet(text, col, SEARCH_SNIPPET_WIDTH));
    }

    // grep fallback (or malformed rg line): best-effort literal locate.
    let col = rest
        .to_lowercase()
        .find(&pattern.to_lowercase())
        .map(|byte| byte + 1)
        .unwrap_or(1);
    (line_num, centered_snippet(rest, col, SEARCH_SNIPPET_WIDTH))
}

// ── Transcript Path Discovery ────────────────────────────────────────────

/// Detect canonical agent type from transcript path.
pub(crate) fn detect_agent_type(path: &str) -> &'static str {
    transcript::agent_name_from_path(path)
}

fn transcript_search_key(path: &str, session_id: Option<&str>) -> String {
    format!("{path}\u{0}{}", session_id.unwrap_or(""))
}

/// Attribute a `--all` disk match to a canonical tool.
///
/// Content detection wins when it lands on a selected tool: it resolves every
/// signatured format and the one shared root (gemini/antigravity under
/// `~/.gemini`). Otherwise the file is attributed by provenance — the search
/// root it was found under — which is what classifies unsignatured sessions such
/// as pi's bare `<uuid>.jsonl` reached via a custom `PI_CODING_AGENT_SESSION_DIR`.
/// Provenance is only trusted when exactly one selected root owns the path, so
/// the shared gemini/antigravity root never guesses.
fn attribute_disk_match(
    file_path: &str,
    selected: &[Tool],
    root_owners: &[(PathBuf, Tool)],
) -> Option<Tool> {
    if let Some(detected) = transcript::detect_tool_from_path(file_path)
        && selected.contains(&detected)
    {
        return Some(detected);
    }
    let path = Path::new(file_path);
    let mut owner: Option<Tool> = None;
    for (root, tool) in root_owners {
        if selected.contains(tool) && path.starts_with(root) {
            match owner {
                None => owner = Some(*tool),
                Some(existing) if existing == *tool => {}
                Some(_) => return None, // ambiguous provenance — do not guess
            }
        }
    }
    owner
}

/// Build an appropriate error message when transcript resolution fails.
/// Uses resolve_display_name_or_stopped (which handles exact base and tag-name
/// resolution) to check if the instance exists without a transcript.
fn no_transcript_error(
    db: &HcomDb,
    name: &str,
    display_name: &str,
    device: Option<&str>,
) -> String {
    if let Some(resolved) = crate::identity::resolve_display_name_or_stopped(db, name) {
        let display_name = if display_name.is_empty() {
            &resolved
        } else {
            display_name
        };
        let command = match device {
            Some(device) => format!(
                "hcom events --remote-fetch --device {device} --participant {resolved} --type message"
            ),
            None => format!("hcom events --participant {resolved} --type message"),
        };
        format!(
            "No model transcript is registered for {display_name}.\nView transport messages with: {command}"
        )
    } else {
        format!("Agent '{display_name}' not found")
    }
}

/// Find a device-suffixed instance name ("dami:KEZE") matching a bare base
/// name ("dami") that has no exact/tag/stopped match of its own. Relay pull
/// always namespaces remote instances with a device suffix (see
/// `relay::add_device_suffix`), so a bare name typed by the user never
/// matches those rows directly — only this prefix lookup does. Without it,
/// callers fall through to a plain-name DB lookup, which still succeeds via
/// a looser `LIKE` prefix match and returns the remote device's transcript
/// path as if it were a local file.
fn resolve_remote_instance_name(db: &HcomDb, base_name: &str) -> Option<String> {
    db.conn()
        .query_row(
            "SELECT name FROM instances WHERE name LIKE ?1 ESCAPE '\\' ORDER BY status_time DESC LIMIT 1",
            rusqlite::params![format!(
                "{}:____",
                base_name.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
            )],
            |row| row.get::<_, String>(0),
        )
        .ok()
}

/// Get exchanges from a transcript file using the shared transcript module.
fn get_exchanges(
    path: &str,
    agent: &str,
    last: usize,
    detailed: bool,
    session_id: Option<&str>,
    retry_codex: bool,
) -> Result<Vec<Exchange>, String> {
    let backend = transcript::backend_from_agent_or_path(agent, path)?;
    let opts = ReadOptions {
        last,
        detailed,
        session_id: session_id.map(|s| s.to_string()),
        allow_codex_retry: retry_codex,
    };
    transcript::read(Path::new(path), backend, &opts)
}

// ── Search ───────────────────────────────────────────────────────────────

/// Correlate transcript file paths to hcom agent names via DB queries.
/// Checks instances table first, then stopped life events.
fn correlate_paths_to_hcom(
    db: &HcomDb,
    targets: &[(String, Option<String>)],
) -> std::collections::HashMap<String, String> {
    let mut result = std::collections::HashMap::new();
    let conn = db.conn();
    let target_keys: std::collections::HashSet<String> = targets
        .iter()
        .map(|(path, session_id)| transcript_search_key(path, session_id.as_deref()))
        .collect();

    // 1. Check current instances
    if let Ok(mut stmt) = conn.prepare(
        "SELECT name, transcript_path, session_id
         FROM instances
         WHERE transcript_path IS NOT NULL",
    ) && let Ok(rows) = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    }) {
        for (name, tp, session_id) in rows.flatten() {
            let key = transcript_search_key(&tp, session_id.as_deref());
            if target_keys.contains(&key) {
                result.insert(key, name);
            }
        }
    }

    // 2. Check stopped events for paths not yet matched
    if let Ok(mut stmt) = conn.prepare(
        "SELECT instance,
                json_extract(data, '$.snapshot.transcript_path') as tp,
                json_extract(data, '$.snapshot.session_id') as session_id \
         FROM events WHERE type = 'life' \
         AND json_extract(data, '$.action') = 'stopped' \
         AND json_extract(data, '$.snapshot.transcript_path') IS NOT NULL \
         ORDER BY id DESC",
    ) && let Ok(rows) = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    }) {
        for (name, tp, session_id) in rows.flatten() {
            let key = transcript_search_key(&tp, session_id.as_deref());
            if target_keys.contains(&key) && !result.contains_key(&key) {
                result.insert(key, name);
            }
        }
    }

    result
}

/// Search across transcripts: `hcom transcript search "pattern" [--live] [--all] [--limit N] [--exclude-self]`
fn cmd_transcript_search(
    db: &HcomDb,
    args: &TranscriptSearchArgs,
    ctx: Option<&CommandContext>,
) -> i32 {
    let live_mode = args.live;
    let all_mode = args.all;
    let json_mode = args.json;
    let limit = args.limit;
    let agent_filter = match args.agent.as_deref() {
        Some(value) => match transcript::parse_tool_filter(value) {
            Ok(tool) => Some(tool),
            Err(error) => {
                eprintln!("Error: {error}");
                return 1;
            }
        },
        None => None,
    };

    // Resolve self name for --exclude-self
    let ctx_name = if args.exclude_self {
        ctx.and_then(|c| c.identity.as_ref())
            .filter(|id| matches!(id.kind, crate::shared::SenderKind::Instance))
            .map(|id| id.name.clone())
    } else {
        None
    };

    let pattern = &args.pattern;

    // Collect transcript paths: (name, path, agent)
    let mut paths: Vec<(String, String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    if all_mode {
        // --all: derive file roots and database sources from canonical tools.
        let selected_tools = agent_filter
            .map(|tool| vec![tool])
            .unwrap_or_else(transcript::transcript_tools);
        let mut search_dirs: Vec<PathBuf> = Vec::new();
        // Remember which tool each search root belongs to so matches found under
        // an override root with no content signature (e.g. pi's bare
        // `<uuid>.jsonl` under a custom PI_CODING_AGENT_SESSION_DIR) can still be
        // attributed. A path can map to more than one tool — gemini and
        // antigravity share `~/.gemini` — which `attribute_disk_match` treats as
        // ambiguous and defers to content detection.
        let mut root_owners: Vec<(PathBuf, Tool)> = Vec::new();
        for tool in &selected_tools {
            for path in transcript::disk_search_roots(*tool) {
                if path.exists() {
                    if !search_dirs.contains(&path) {
                        search_dirs.push(path.clone());
                    }
                    root_owners.push((path, *tool));
                }
            }
        }
        let database_sources: Vec<(Tool, PathBuf)> = selected_tools
            .iter()
            .filter_map(|tool| transcript::database_search_path(*tool).map(|path| (*tool, path)))
            .collect();

        if search_dirs.is_empty() && database_sources.is_empty() {
            println!("No transcript directories or databases found on disk.");
            return 0;
        }

        // Phase 1: find matching files with rg -l (recursive, *.jsonl/*.json).
        // Avoid invoking rg without a path when only SQLite sources exist; that
        // would make it read stdin and potentially block an interactive command.
        let matching_files: Vec<String> = if search_dirs.is_empty() {
            Vec::new()
        } else {
            let mut cmd = std::process::Command::new("rg");
            cmd.args(["-l", "--glob", "*.jsonl", "--glob", "*.json", pattern]);
            for d in &search_dirs {
                cmd.arg(d);
            }
            match cmd.output() {
                Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect(),
                Ok(out) if out.status.code() == Some(1) => Vec::new(),
                Ok(out) => {
                    eprintln!(
                        "Error: ripgrep failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    );
                    return 1;
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    eprintln!("Error: transcript search --all requires `rg` (ripgrep) on PATH");
                    return 1;
                }
                Err(err) => {
                    eprintln!("Error: could not run `rg`: {err}");
                    return 1;
                }
            }
        };

        let mut database_matches = Vec::new();
        for (tool, db_path) in &database_sources {
            if database_matches.len() >= limit {
                break;
            }
            match transcript::search_database_sessions(
                *tool,
                db_path,
                pattern,
                limit - database_matches.len(),
            ) {
                Ok(matches) => database_matches.extend(matches),
                Err(err) => {
                    eprintln!("Error: {err}");
                    return 1;
                }
            }
        }

        if matching_files.is_empty() && database_matches.is_empty() {
            if json_mode {
                println!("{}", json!({"count": 0, "results": [], "scope": "all"}));
            } else {
                println!("No matches for \"{pattern}\"");
            }
            return 0;
        }

        // Correlate transcript paths/session IDs to hcom names via DB.
        let mut targets: Vec<(String, Option<String>)> = matching_files
            .iter()
            .cloned()
            .map(|path| (path, None))
            .collect();
        targets.extend(
            database_matches
                .iter()
                .filter_map(|m| m.session_id.clone().map(|sid| (m.path.clone(), Some(sid)))),
        );
        let path_to_hcom = correlate_paths_to_hcom(db, &targets);

        // Extract line-level matches from each file
        let mut results = Vec::new();
        for file_path in &matching_files {
            if results.len() >= limit {
                break;
            }
            let Some(detected_tool) =
                attribute_disk_match(file_path, &selected_tools, &root_owners)
            else {
                continue;
            };
            let agent = detected_tool.as_str();
            let hcom_name = path_to_hcom
                .get(&transcript_search_key(file_path, None))
                .cloned()
                .unwrap_or_default();

            let remaining = limit - results.len();
            let max_count = remaining.to_string();
            let out = match run_search_tool(
                "rg",
                &[
                    "-n",
                    "--column",
                    "--max-count",
                    &max_count,
                    pattern,
                    file_path,
                ],
            ) {
                Ok(output) => output,
                Err(err) => {
                    eprintln!("Error: {err}");
                    return 1;
                }
            };
            if let Some(out) = out {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let lines: Vec<&str> = stdout.lines().collect();
                let match_count = lines.len();
                if match_count > 0 {
                    let (line_num, snippet) = parse_match_line(lines[0], true, pattern);

                    results.push(json!({
                            "hcom_name": if hcom_name.is_empty() { serde_json::Value::Null } else { json!(hcom_name) },
                            "agent": agent,
                            "path": file_path,
                            "line": line_num,
                            "text": snippet,
                            "matches": match_count,
                        }));
                }
            }
        }

        for database_match in &database_matches {
            if results.len() >= limit {
                break;
            }
            let hcom_name = path_to_hcom
                .get(&transcript_search_key(
                    &database_match.path,
                    database_match.session_id.as_deref(),
                ))
                .cloned()
                .unwrap_or_default();
            results.push(json!({
                "hcom_name": if hcom_name.is_empty() { serde_json::Value::Null } else { json!(hcom_name) },
                "agent": database_match.agent,
                "path": database_match.path,
                "line": database_match.line,
                "text": database_match.text,
                "matches": database_match.matches,
                "session_id": database_match.session_id,
                "label": database_match.label,
            }));
        }

        if json_mode {
            println!(
                "{}",
                json!({"count": results.len(), "results": results, "scope": "all"})
            );
        } else if results.is_empty() {
            println!("No matches for \"{pattern}\"");
        } else {
            println!(
                "Found matches in {} transcripts (all on disk):",
                results.len()
            );
            for r in &results {
                let path = r["path"].as_str().unwrap_or("");
                let agent = r["agent"].as_str().unwrap_or("?");
                let line = r["line"].as_u64().unwrap_or(0);
                let matches = r["matches"].as_u64().unwrap_or(0);
                let snippet = r["text"].as_str().unwrap_or("");
                let label = r["label"].as_str().unwrap_or("");
                let session_id = r["session_id"].as_str().unwrap_or("");
                let short_path = path
                    .split('/')
                    .rev()
                    .take(3)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("/");
                let name_part = r["hcom_name"]
                    .as_str()
                    .map(|n| format!(" ({n})"))
                    .unwrap_or_default();
                println!("  [{agent}]{name_part} .../{short_path}:{line}  ({matches} matches)");
                if !label.is_empty() || !session_id.is_empty() {
                    let mut details = Vec::new();
                    if !label.is_empty() {
                        details.push(label.to_string());
                    }
                    if !session_id.is_empty() {
                        details.push(session_id.to_string());
                    }
                    println!("    {}", details.join(" | "));
                }
                if !snippet.is_empty() {
                    println!("    {snippet}");
                }
            }
        }
        return 0;
    } else {
        // Active instances
        if let Ok(mut stmt) = db.conn().prepare(
            "SELECT name, transcript_path, tool FROM instances WHERE transcript_path IS NOT NULL AND transcript_path != ''"
        )
            && let Ok(rows) = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            }) {
                for (name, path, tool) in rows.flatten() {
                    if let Some(filter_tool) = agent_filter
                        && transcript::tool_from_agent_or_path(&tool, &path).ok() != Some(filter_tool)
                    {
                        continue;
                    }
                    if args.exclude_self && ctx_name.as_deref() == Some(name.as_str()) { continue; }
                    seen.insert(name.clone());
                    paths.push((name, path, tool));
                }
            }

        // Stopped instances from life event snapshots (C2/C3 fix)
        if !live_mode
            && let Ok(mut stmt) = db.conn().prepare(
                "SELECT instance, json_extract(data, '$.snapshot.transcript_path'), json_extract(data, '$.snapshot.tool') FROM events WHERE type = 'life' AND json_extract(data, '$.action') = 'stopped' AND json_extract(data, '$.snapshot.transcript_path') IS NOT NULL"
            )
                && let Ok(rows) = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                }) {
                    for (name, path, tool) in rows.flatten() {
                        if seen.contains(&name) { continue; }
                        if let Some(filter_tool) = agent_filter
                            && transcript::tool_from_agent_or_path(&tool, &path).ok() != Some(filter_tool)
                        {
                            continue;
                        }
                        if args.exclude_self && ctx_name.as_deref() == Some(name.as_str()) { continue; }
                        seen.insert(name.clone());
                        paths.push((name, path, tool));
                    }
                }
    }

    // Search using ripgrep (with line-level matches + snippets) — hcom-tracked/live paths
    let mut results = Vec::new();
    for (name, path, agent) in &paths {
        if !Path::new(path).exists() {
            continue;
        }

        // Use rg for line-level matches with context. `--column` gives us the
        // match offset so the snippet can be centered on the hit; the grep
        // fallback has no column, so parse_match_line locates the pattern itself.
        let remaining = limit - results.len();
        let max_count = remaining.to_string();
        let mut has_column = true;
        let output = match run_search_tool(
            "rg",
            &["-n", "--column", "--max-count", &max_count, pattern, path],
        ) {
            Ok(output) => Ok(output),
            Err(rg_err) if rg_err.contains("was not found on PATH") => {
                has_column = false;
                run_search_tool("grep", &["-n", "-m", &max_count, pattern, path]).map_err(
                    |grep_err| {
                        format!(
                            "transcript search requires `rg` or `grep` on PATH ({rg_err}; {grep_err})"
                        )
                    },
                )
            }
            Err(err) => Err(err),
        };

        let output = match output {
            Ok(output) => output,
            Err(err) => {
                eprintln!("Error: {err}");
                return 1;
            }
        };

        if let Some(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let lines: Vec<&str> = stdout.lines().collect();
            let match_count = lines.len();
            if match_count > 0 {
                let (line_num, snippet) = parse_match_line(lines[0], has_column, pattern);

                results.push(json!({
                    "hcom_name": name,
                    "agent": agent,
                    "path": path,
                    "line": line_num,
                    "text": snippet,
                    "matches": match_count,
                }));
            }
        }

        if results.len() >= limit {
            break;
        }
    }

    let scope_label = if live_mode {
        " (live agents)"
    } else if all_mode {
        ""
    } else {
        " (hcom-tracked)"
    };

    if json_mode {
        println!(
            "{}",
            json!({"count": results.len(), "results": results, "scope": if live_mode {"live"} else if all_mode {"all"} else {"hcom"}})
        );
    } else {
        if results.is_empty() {
            println!("No matches for \"{pattern}\"");
            return 0;
        }
        let limit_hit = results.len() >= limit;
        if limit_hit {
            println!(
                "Showing {} matches (limit {}){scope_label}:\n",
                results.len(),
                limit
            );
        } else {
            println!("Found {} matches{scope_label}:\n", results.len());
        }
        for result in &results {
            let hcom_name = result
                .get("hcom_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let agent = result.get("agent").and_then(|v| v.as_str()).unwrap_or("");
            let path = result.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let line = result.get("line").and_then(|v| v.as_u64()).unwrap_or(0);
            let snippet = result.get("text").and_then(|v| v.as_str()).unwrap_or("");

            let path_display = if path.len() > 60 {
                let mut start = path.len() - 57;
                while start < path.len() && !path.is_char_boundary(start) {
                    start += 1;
                }
                format!("...{}", &path[start..])
            } else {
                path.to_string()
            };

            println!("[{agent}:{hcom_name}] {path_display}:{line}");
            // Snippet is already bounded and centered on the match by
            // parse_match_line; just flatten newlines for single-line display.
            let snippet_clean = snippet.replace('\n', " ");
            println!("    {snippet_clean}\n");
        }
    }

    0
}

/// Timeline: `hcom transcript timeline [--last N] [--full] [--json]`
fn cmd_transcript_timeline(db: &HcomDb, args: &TranscriptTimelineArgs) -> i32 {
    let json_mode = args.json;
    let full_mode = args.full;
    let detailed = args.detailed;
    let last_n = args.last.unwrap_or(10);

    // Collect all transcript paths (active + stopped sessions, C3 fix)
    let mut all_entries: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Active instances
    if let Ok(mut stmt) = db.conn().prepare(
        "SELECT name, transcript_path, tool, session_id FROM instances WHERE transcript_path IS NOT NULL AND transcript_path != ''"
    )
        && let Ok(rows) = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        }) {
            for (name, path, tool, sid) in rows.flatten() {
                seen.insert(name.clone());
                if let Ok(exchanges) =
                    get_exchanges(&path, &tool, last_n, detailed, sid.as_deref(), true)
                {
                    for ex in exchanges {
                        all_entries.push(json!({
                            "instance": name,
                            "position": ex.position,
                            "user": ex.user,
                            "action": if full_mode { ex.action.clone() } else { summarize_action(&ex.action) },
                            "timestamp": ex.timestamp,
                            "files": ex.files,
                        }));
                    }
                }
            }
        }

    // Stopped instances from life event snapshots
    if let Ok(mut stmt) = db.conn().prepare(
        "SELECT instance, json_extract(data, '$.snapshot.transcript_path'), json_extract(data, '$.snapshot.tool'), json_extract(data, '$.snapshot.session_id') FROM events WHERE type = 'life' AND json_extract(data, '$.action') = 'stopped' AND json_extract(data, '$.snapshot.transcript_path') IS NOT NULL"
    )
        && let Ok(rows) = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        }) {
            for (name, path, tool, sid) in rows.flatten() {
                if seen.contains(&name) { continue; }
                seen.insert(name.clone());
                if let Ok(exchanges) =
                    get_exchanges(&path, &tool, last_n, detailed, sid.as_deref(), true)
                {
                    for ex in exchanges {
                        all_entries.push(json!({
                            "instance": name,
                            "position": ex.position,
                            "user": ex.user,
                            "action": if full_mode { ex.action.clone() } else { summarize_action(&ex.action) },
                            "timestamp": ex.timestamp,
                            "files": ex.files,
                        }));
                    }
                }
            }
        }

    // Sort by timestamp (most recent first)
    all_entries.sort_by(|a, b| {
        let ts_a = a.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        let ts_b = b.get("timestamp").and_then(|v| v.as_str()).unwrap_or("");
        ts_b.cmp(ts_a) // Reverse order
    });

    // Limit
    if all_entries.len() > last_n {
        all_entries.truncate(last_n);
    }

    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(&all_entries).unwrap_or_default()
        );
        return 0;
    }

    if all_entries.is_empty() {
        println!("No transcript entries found");
        return 0;
    }

    //
    println!("Timeline ({} exchanges):\n", all_entries.len());
    for entry in &all_entries {
        let inst = entry.get("instance").and_then(|v| v.as_str()).unwrap_or("");
        let ts = entry
            .get("timestamp")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let user = entry.get("user").and_then(|v| v.as_str()).unwrap_or("");
        let action = entry.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let files = entry.get("files").and_then(|v| v.as_array());

        let ts_short = if ts.contains('T') {
            ts.get(11..16).unwrap_or("??:??")
        } else if ts.len() >= 5 {
            ts.get(..5).unwrap_or("??:??")
        } else {
            "??:??"
        };

        let user_display = if user.len() > 80 {
            format!("{}...", truncate_str(user, 77))
        } else {
            user.to_string()
        };

        println!("[{ts_short}] \"{user_display}\"");

        if full_mode {
            for action_line in action.lines().take(10) {
                println!("  {action_line}");
            }
            let line_count = action.lines().count();
            if line_count > 10 {
                println!("  ... (+{} lines)", line_count - 10);
            }
        } else {
            let action_short = summarize_action(action);
            let action_display = if action_short.len() > 100 {
                format!("{}...", truncate_str(&action_short, 97))
            } else {
                action_short
            };
            println!("  → {action_display}");
        }

        if let Some(file_arr) = files {
            let file_strs: Vec<&str> = file_arr.iter().take(5).filter_map(|v| v.as_str()).collect();
            if !file_strs.is_empty() {
                println!("  Files: {}", file_strs.join(", "));
            }
        }

        // Command line (instance reference for navigation)
        println!(
            "  hcom transcript @{inst} {}",
            entry.get("position").and_then(|v| v.as_u64()).unwrap_or(1)
        );
        println!();
    }

    0
}

// ── Main Entry Point ─────────────────────────────────────────────────────

/// Main entry point for `hcom transcript` command.
pub fn cmd_transcript(db: &HcomDb, args: &TranscriptArgs, ctx: Option<&CommandContext>) -> i32 {
    // Handle subcommands
    match &args.subcmd {
        Some(TranscriptSubcmd::Search(search_args)) => {
            return cmd_transcript_search(db, search_args, ctx);
        }
        Some(TranscriptSubcmd::Timeline(timeline_args)) => {
            return cmd_transcript_timeline(db, timeline_args);
        }
        None => {}
    }

    let json_mode = args.json;
    let full_mode = args.full;
    let detailed = args.detailed;
    let last_n = args.last.unwrap_or(10);

    if let Some(ref name) = args.name {
        let stripped = name.strip_prefix('@').unwrap_or(name);
        let resolved = crate::identity::resolve_display_name_or_stopped(db, stripped)
            .or_else(|| resolve_remote_instance_name(db, stripped))
            .unwrap_or_else(|| stripped.to_string());
        if let Some((base_name, device)) = crate::relay::control::split_device_suffix(&resolved) {
            return crate::relay::control::dispatch_remote_and_print(
                db,
                device,
                Some(&resolved),
                crate::relay::control::rpc_action::TRANSCRIPT,
                &json!({
                    "target": base_name,
                    "display_target": resolved,
                    "origin_device": device,
                    "last": last_n,
                    "range": args.range_flag.as_ref().or(args.range_positional.as_ref()),
                    "json": json_mode,
                    "full": full_mode,
                    "detailed": detailed,
                }),
                crate::relay::control::RPC_DEFAULT_TIMEOUT,
                "content",
                "No remote transcript content",
            );
        }
    }

    // Resolve target and range from positional args
    let mut target = None;
    let mut range_str: Option<String> = args.range_flag.clone();

    if let Some(ref name) = args.name {
        let stripped = name.strip_prefix('@').unwrap_or(name);
        // Check if it looks like a range (digits and hyphens)
        if stripped.chars().all(|c| c.is_ascii_digit() || c == '-')
            && stripped.chars().any(|c| c.is_ascii_digit())
        {
            if range_str.is_none() {
                range_str = Some(stripped.to_string());
            }
        } else {
            target = Some(stripped.to_string());
        }
    }

    if let Some(ref range_pos) = args.range_positional
        && range_str.is_none()
    {
        range_str = Some(range_pos.clone());
    }

    // Resolve target to transcript path
    let (instance_name, transcript_path, agent_type, session_id) = if let Some(ref name) = target {
        // Try direct match
        let resolved = resolve_instance_transcript(db, name);
        match resolved {
            Some(r) => r,
            None => {
                eprintln!("Error: {}", no_transcript_error(db, name, name, None));
                return 1;
            }
        }
    } else if let Some(id) = ctx.and_then(|c| c.identity.as_ref()) {
        // Default to self
        match resolve_instance_transcript(db, &id.name) {
            Some(r) => r,
            None => {
                eprintln!("Error: No transcript available for current instance");
                return 1;
            }
        }
    } else {
        eprintln!("Usage: hcom transcript @instance [N | N-M] [--full] [--json]");
        return 1;
    };

    // Parse range
    let (range_start, range_end) = if let Some(ref r) = range_str {
        parse_range(r)
    } else {
        (None, None)
    };

    // Get exchanges
    let effective_last = if range_start.is_some() {
        usize::MAX
    } else {
        last_n
    };
    let exchanges = match get_exchanges(
        &transcript_path,
        &agent_type,
        effective_last,
        detailed,
        session_id.as_deref(),
        true,
    ) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Error: {e}");
            return 1;
        }
    };

    // Apply range filter
    let filtered: Vec<&Exchange> = if let Some(start) = range_start {
        let end = range_end.unwrap_or(start);
        exchanges
            .iter()
            .filter(|e| e.position >= start && e.position <= end)
            .collect()
    } else {
        exchanges.iter().collect()
    };

    if json_mode {
        let json_output: Vec<Value> = filtered
            .iter()
            .map(|ex| {
                let mut obj = json!({
                    "position": ex.position,
                    "user": ex.user,
                    "action": ex.action,
                    "files": ex.files,
                    "timestamp": ex.timestamp,
                });
                if detailed {
                    obj["tools"] = json!(
                        ex.tools
                            .iter()
                            .map(|t| {
                                let mut tool = json!({
                                    "name": t.name,
                                    "is_error": t.is_error,
                                });
                                if let Some(ref f) = t.file {
                                    tool["file"] = json!(f);
                                }
                                if let Some(ref c) = t.command {
                                    tool["command"] = json!(c);
                                }
                                tool
                            })
                            .collect::<Vec<_>>()
                    );
                    obj["edits"] = json!(ex.edits);
                    obj["errors"] = json!(ex.errors);
                    obj["ended_on_error"] = json!(ex.ended_on_error);
                }
                obj
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json_output).unwrap_or_default()
        );
        return 0;
    }

    if filtered.is_empty() {
        println!("No exchanges found");
        return 0;
    }

    // Header: "Recent conversation (N exchanges, X-Y of Z) - @instance:"
    let first_pos = filtered.first().map(|e| e.position).unwrap_or(1);
    let last_pos = filtered.last().map(|e| e.position).unwrap_or(1);
    println!(
        "Recent conversation ({} exchanges, {}-{} of {}) - @{}:\n",
        filtered.len(),
        first_pos,
        last_pos,
        exchanges.len(),
        instance_name,
    );

    let owned: Vec<Exchange> = filtered.into_iter().cloned().collect();
    let formatted = format_exchanges(&owned, &instance_name, full_mode, detailed);
    println!("{formatted}");

    0
}

/// Display options for transcript rendering.
pub struct TranscriptRenderOpts<'a> {
    pub range: Option<&'a str>,
    pub last_n: usize,
    pub json_mode: bool,
    pub full_mode: bool,
    pub detailed: bool,
    pub retry_codex: bool,
}

impl Default for TranscriptRenderOpts<'_> {
    fn default() -> Self {
        Self {
            range: None,
            last_n: 10,
            json_mode: false,
            full_mode: false,
            detailed: false,
            retry_codex: true,
        }
    }
}

pub fn render_instance_transcript(
    db: &HcomDb,
    name: &str,
    last_n: usize,
) -> Result<String, String> {
    render_instance_transcript_impl(
        db,
        name,
        &TranscriptRenderOpts {
            last_n,
            ..Default::default()
        },
        name,
        None,
    )
}

pub fn render_instance_transcript_with_options_no_retry(
    db: &HcomDb,
    name: &str,
    range: Option<&str>,
    last_n: usize,
    json_mode: bool,
    full_mode: bool,
    detailed: bool,
) -> Result<String, String> {
    render_instance_transcript_impl(
        db,
        name,
        &TranscriptRenderOpts {
            range,
            last_n,
            json_mode,
            full_mode,
            detailed,
            retry_codex: false,
        },
        name,
        None,
    )
}

/// Render a remote transcript while retaining the caller's device-qualified
/// name in diagnostics.
pub fn render_remote_instance_transcript_with_options_no_retry(
    db: &HcomDb,
    name: &str,
    display_name: &str,
    device: &str,
    opts: &TranscriptRenderOpts<'_>,
) -> Result<String, String> {
    render_instance_transcript_impl(db, name, opts, display_name, Some(device))
}

pub fn render_instance_transcript_with_options(
    db: &HcomDb,
    name: &str,
    range: Option<&str>,
    last_n: usize,
    json_mode: bool,
    full_mode: bool,
    detailed: bool,
) -> Result<String, String> {
    render_instance_transcript_impl(
        db,
        name,
        &TranscriptRenderOpts {
            range,
            last_n,
            json_mode,
            full_mode,
            detailed,
            retry_codex: true,
        },
        name,
        None,
    )
}

fn render_instance_transcript_impl(
    db: &HcomDb,
    name: &str,
    opts: &TranscriptRenderOpts<'_>,
    display_name: &str,
    device: Option<&str>,
) -> Result<String, String> {
    let (instance_name, transcript_path, agent_type, session_id) =
        resolve_instance_transcript(db, name)
            .ok_or_else(|| no_transcript_error(db, name, display_name, device))?;
    let (range_start, range_end) = if let Some(r) = opts.range {
        parse_range(r)
    } else {
        (None, None)
    };
    let effective_last = if range_start.is_some() {
        usize::MAX
    } else {
        opts.last_n
    };
    let exchanges = get_exchanges(
        &transcript_path,
        &agent_type,
        effective_last,
        opts.detailed,
        session_id.as_deref(),
        opts.retry_codex,
    )
    .map_err(|e| e.to_string())?;

    let filtered: Vec<&Exchange> = if let Some(start) = range_start {
        let end = range_end.unwrap_or(start);
        exchanges
            .iter()
            .filter(|e| e.position >= start && e.position <= end)
            .collect()
    } else {
        exchanges.iter().collect()
    };

    if opts.json_mode {
        let json_output: Vec<Value> = filtered
            .iter()
            .map(|ex| {
                let mut obj = json!({
                    "position": ex.position,
                    "user": ex.user,
                    "action": ex.action,
                    "files": ex.files,
                    "timestamp": ex.timestamp,
                });
                if opts.detailed {
                    obj["tools"] = json!(
                        ex.tools
                            .iter()
                            .map(|t| {
                                let mut tool = json!({
                                    "name": t.name,
                                    "is_error": t.is_error,
                                });
                                if let Some(ref f) = t.file {
                                    tool["file"] = json!(f);
                                }
                                if let Some(ref c) = t.command {
                                    tool["command"] = json!(c);
                                }
                                tool
                            })
                            .collect::<Vec<_>>()
                    );
                    obj["edits"] = json!(ex.edits);
                    obj["errors"] = json!(ex.errors);
                    obj["ended_on_error"] = json!(ex.ended_on_error);
                }
                obj
            })
            .collect();
        return serde_json::to_string_pretty(&json_output).map_err(|e| e.to_string());
    }

    if filtered.is_empty() {
        return Ok("No exchanges found".to_string());
    }

    let first_pos = filtered.first().map(|e| e.position).unwrap_or(1);
    let last_pos = filtered.last().map(|e| e.position).unwrap_or(1);
    let owned: Vec<Exchange> = filtered.into_iter().cloned().collect();
    let formatted = format_exchanges(&owned, &instance_name, opts.full_mode, opts.detailed);
    Ok(format!(
        "Recent conversation ({} exchanges, {}-{} of {}) - @{}:\n\n{}",
        owned.len(),
        first_pos,
        last_pos,
        exchanges.len(),
        instance_name,
        formatted
    ))
}

/// Resolve instance name to (name, transcript_path, agent_type, session_id).
fn resolve_instance_transcript(
    db: &HcomDb,
    name: &str,
) -> Option<(String, String, String, Option<String>)> {
    // An exact live or stopped identity is authoritative even when it has no
    // transcript. Only infer a prefix when no exact identity exists; otherwise
    // a transcript-bearing longer name can disclose the wrong conversation.
    if let Some(exact_name) = crate::identity::resolve_display_name_or_stopped(db, name) {
        match db.get_instance_full(&exact_name) {
            Ok(Some(instance)) if !instance.transcript_path.is_empty() => {
                return Some((
                    exact_name,
                    instance.transcript_path,
                    instance.tool,
                    instance.session_id,
                ));
            }
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) => {}
        }

        if let Ok((path, sid)) = db.conn().query_row(
            "SELECT json_extract(data, '$.snapshot.transcript_path'), json_extract(data, '$.snapshot.session_id') FROM events WHERE type = 'life' AND instance = ? AND json_extract(data, '$.action') = 'stopped' ORDER BY id DESC LIMIT 1",
            rusqlite::params![&exact_name],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        ) {
            let agent = detect_agent_type(&path).to_string();
            return Some((exact_name, path, agent, sid));
        }

        return None;
    }

    // Prefix match (literal matching; only an unambiguous single match returns immediately)
    let escaped = name
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    let pattern = format!("{escaped}%");

    if let Ok(mut stmt) = db.conn().prepare(
        "SELECT name, transcript_path, tool, session_id FROM instances WHERE name LIKE ?1 ESCAPE '\\' AND transcript_path IS NOT NULL AND transcript_path != '' LIMIT 2",
    ) {
        let rows_res = stmt
            .query_map(rusqlite::params![pattern], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .and_then(|mapped| mapped.collect::<rusqlite::Result<Vec<_>>>());

        if let Ok(matches) = rows_res
            && matches.len() == 1
        {
            return Some(matches.into_iter().next().unwrap());
        }
    }

    None
}

/// Parse range string "N-M" or "N".
fn parse_range(s: &str) -> (Option<usize>, Option<usize>) {
    if let Some(dash_pos) = s.find('-') {
        let start: Option<usize> = s[..dash_pos].parse().ok().filter(|&v: &usize| v >= 1);
        let end: Option<usize> = s[dash_pos + 1..].parse().ok().filter(|&v: &usize| v >= 1);
        // Validate start <= end
        if let (Some(s), Some(e)) = (start, end)
            && s > e
        {
            eprintln!("Error: invalid range '{s}-{e}' (start must be <= end)");
            return (None, None);
        }
        (start, end)
    } else {
        let pos: Option<usize> = s.parse().ok().filter(|&v: &usize| v >= 1);
        (pos, pos)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod tests;
