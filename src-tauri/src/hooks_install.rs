//! Installs/removes Claude Companion's hooks in ~/.claude/settings.json.
//!
//! Every event is routed through one small script that forwards the hook JSON
//! to the app. Only entries whose command points at that script are touched;
//! everything else in settings.json is left as-is.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Map, Value};

use crate::server::PORT;

const SCRIPT_NAME: &str = "hook.sh";
/// (event, hook timeout in seconds)
const EVENTS: &[(&str, u64)] = &[
    ("SessionStart", 5),
    ("UserPromptSubmit", 5),
    ("PreToolUse", 5),
    ("PostToolUse", 5),
    ("PermissionRequest", 600),
    ("Notification", 5),
    ("Stop", 5),
    ("SessionEnd", 2),
];

fn script_path() -> Result<PathBuf, String> {
    Ok(dirs::home_dir().ok_or("no home directory")?.join(".claude-companion").join(SCRIPT_NAME))
}

fn settings_path() -> Result<PathBuf, String> {
    Ok(dirs::home_dir().ok_or("no home directory")?.join(".claude").join("settings.json"))
}

fn script_body() -> String {
    format!(
        r#"#!/bin/sh
# Claude Companion hook: forwards the Claude Code hook event (JSON on stdin) to
# the menu bar app and prints its reply (a permission decision, or nothing).
# When the app isn't running, curl fails fast and Claude Code carries on.
curl -sf --max-time 590 -X POST \
  -H 'Content-Type: application/json' \
  -H "X-CC-PID: $PPID" \
  -H "X-CC-Term: ${{TERM_PROGRAM:-}}" \
  -H "X-CC-Bundle: ${{__CFBundleIdentifier:-}}" \
  -H "X-CC-Iterm: ${{ITERM_SESSION_ID:-}}" \
  --data-binary @- "http://127.0.0.1:{PORT}/hook" || true
"#
    )
}

fn is_ours(group: &Value, script: &str) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks
                .iter()
                .any(|h| h.get("command").and_then(Value::as_str).is_some_and(|c| c.contains(script)))
        })
}

fn read_settings() -> Result<(PathBuf, Map<String, Value>), String> {
    let path = settings_path()?;
    let settings = match fs::read_to_string(&path) {
        Ok(text) if !text.trim().is_empty() => match serde_json::from_str(&text) {
            Ok(Value::Object(map)) => map,
            _ => return Err(format!("{} is not a JSON object; not touching it", path.display())),
        },
        _ => Map::new(),
    };
    Ok((path, settings))
}

fn write_settings(path: &PathBuf, settings: &Map<String, Value>) -> Result<(), String> {
    if path.exists() {
        let backup = path.with_extension("json.companion-bak");
        fs::copy(path, &backup).map_err(|e| format!("backup failed: {e}"))?;
    } else if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(path, text + "\n").map_err(|e| e.to_string())
}

/// Removes our matcher groups from every event, dropping events left empty.
fn strip_ours(settings: &mut Map<String, Value>, script: &str) {
    let Some(Value::Object(hooks)) = settings.get_mut("hooks") else { return };
    for groups in hooks.values_mut() {
        if let Value::Array(groups) = groups {
            groups.retain(|g| !is_ours(g, script));
        }
    }
    hooks.retain(|_, groups| !groups.as_array().is_some_and(Vec::is_empty));
    if hooks.is_empty() {
        settings.remove("hooks");
    }
}

pub fn install() -> Result<(), String> {
    let script = script_path()?;
    fs::create_dir_all(script.parent().unwrap()).map_err(|e| e.to_string())?;
    fs::write(&script, script_body()).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }

    let script_str = script.to_string_lossy().into_owned();
    let (path, mut settings) = read_settings()?;
    strip_ours(&mut settings, &script_str);

    let hooks = settings
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("\"hooks\" in settings.json is not an object")?;
    for (event, timeout) in EVENTS {
        let groups = hooks.entry(*event).or_insert_with(|| Value::Array(vec![]));
        let Value::Array(groups) = groups else {
            return Err(format!("hooks.{event} in settings.json is not an array"));
        };
        groups.push(json!({
            "hooks": [{
                "type": "command",
                "command": format!("'{script_str}'"),
                "timeout": timeout
            }]
        }));
    }
    write_settings(&path, &settings)
}

pub fn uninstall() -> Result<(), String> {
    let script = script_path()?;
    let (path, mut settings) = read_settings()?;
    strip_ours(&mut settings, &script.to_string_lossy());
    write_settings(&path, &settings)
}

/// True until hooks have been installed once; removing them later via the
/// menu leaves the script behind, so we don't silently reinstall.
pub fn is_first_run() -> bool {
    script_path().is_ok_and(|p| !p.exists())
}

pub fn is_installed() -> bool {
    let (Ok(script), Ok((_, settings))) = (script_path(), read_settings()) else { return false };
    let script = script.to_string_lossy();
    settings
        .get("hooks")
        .and_then(|h| h.get("PermissionRequest"))
        .and_then(Value::as_array)
        .is_some_and(|groups| groups.iter().any(|g| is_ours(g, &script)))
}
