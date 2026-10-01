//! Antigravity plugin packaging (the only tool packaged as a plugin; the rest
//! load hooks per launch or from `hooks.json`).

use std::path::Path;

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn json(path: &Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn agy_hooks_route_to_hcom_gemini_handlers() {
    let hooks = json(&root().join("plugin/hcom-agy/hooks/hooks.json"));
    let text = hooks.to_string();
    for handler in [
        "gemini-sessionstart",
        "gemini-beforeagent",
        "gemini-afteragent",
        "gemini-beforetool",
        "gemini-aftertool",
        "gemini-sessionend",
    ] {
        assert!(text.contains(handler), "AGY hooks.json lacks {handler}");
    }
    assert!(
        root()
            .join("plugin/hcom-agy/.claude-plugin/plugin.json")
            .is_file()
    );
}
