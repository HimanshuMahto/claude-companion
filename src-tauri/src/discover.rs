//! Finds running Claude Code sessions from `~/.claude/sessions/<pid>.json`,
//! which Claude Code keeps up to date with each session's status. This picks
//! up sessions that started before the app (or haven't fired a hook yet).
//! The file format is undocumented, so everything here is best-effort; hooks
//! still drive the permission popups.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::focus;
use crate::server;
use crate::state::{now_ms, AppState, Inner, Session, Status, Terminal};
use crate::ui;

const SCAN_EVERY: Duration = Duration::from_secs(2);
/// How much of the end of a transcript to search for the pending tool call.
const TRANSCRIPT_TAIL: u64 = 256 * 1024;
/// A session we never matched to a process is dropped after this long without a hook.
const UNMATCHED_TIMEOUT_MS: u64 = 10 * 60 * 1000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionFile {
    pid: u32,
    session_id: String,
    cwd: String,
    #[serde(default)]
    kind: String,
    name: Option<String>,
    status: Option<String>,
    waiting_for: Option<String>,
    status_updated_at: Option<u64>,
}

pub fn spawn(app: AppHandle) {
    std::thread::spawn(move || loop {
        scan(&app);
        std::thread::sleep(SCAN_EVERY);
    });
}

fn read_files() -> Vec<SessionFile> {
    let Some(dir) = dirs::home_dir().map(|h| h.join(".claude").join("sessions")) else { return vec![] };
    let Ok(entries) = fs::read_dir(dir) else { return vec![] };
    entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_str::<SessionFile>(&fs::read_to_string(e.path()).ok()?).ok())
        // Skip headless `claude -p` runs and leftovers from exited sessions.
        .filter(|f| (f.kind.is_empty() || f.kind == "interactive") && focus::is_alive(f.pid))
        .collect()
}

fn scan(app: &AppHandle) {
    let files = read_files();
    let state = app.state::<AppState>();

    // Terminal lookups shell out to `ps`, so do them before taking the lock.
    let known: HashSet<u32> = {
        let inner = state.0.lock().unwrap();
        inner.sessions.iter().filter_map(|s| s.terminal.claude_pid.filter(|_| s.terminal.tty.is_some())).collect()
    };
    let terminals: Vec<Option<Terminal>> =
        files.iter().map(|f| (!known.contains(&f.pid)).then(|| focus::process_terminal(f.pid))).collect();

    let mut changed = false;
    let mut finished = Vec::new();
    let mut inner = state.0.lock().unwrap();
    changed |= prune_ended(&mut inner);
    for (f, terminal) in files.iter().zip(terminals) {
        let file_status = match f.status.as_deref() {
            Some("busy") => Status::Working,
            Some("waiting") => Status::Waiting,
            _ => Status::Idle,
        };
        let has_popup = inner.pending.iter().any(|p| p.view.session_id == f.session_id && p.view.kind != "done");

        let idx = inner
            .sessions
            .iter()
            .position(|s| s.id == f.session_id)
            .or_else(|| inner.sessions.iter().position(|s| s.terminal.claude_pid == Some(f.pid)));
        let Some(idx) = idx else {
            inner.sessions.push(Session {
                id: f.session_id.clone(),
                cwd: f.cwd.clone(),
                name: f.name.clone(),
                quiet: false,
                waiting_for: f.waiting_for.clone(),
                last_prompt: None,
                last_seen: 0,
                status: file_status,
                since: now_ms(),
                terminal: terminal.unwrap_or_else(|| Terminal { claude_pid: Some(f.pid), ..Default::default() }),
            });
            changed = true;
            continue;
        };

        let s = &mut inner.sessions[idx];
        if s.id != f.session_id || s.cwd != f.cwd || s.name != f.name {
            // After /clear the same process carries on under a new session id.
            s.id = f.session_id.clone();
            s.cwd = f.cwd.clone();
            s.name = f.name.clone();
            changed = true;
        }
        if let Some(t) = terminal {
            s.terminal = t;
        }
        s.waiting_for = f.waiting_for.clone();
        // Hooks report faster than the file; only trust the file when it's newer.
        if has_popup || f.status_updated_at.unwrap_or(0) <= s.since {
            continue;
        }
        let next = match (s.status, file_status) {
            (Status::Working | Status::Done, Status::Idle) => Status::Done,
            (_, st) => st,
        };
        if next != s.status {
            // Sessions without our hooks only show they finished through this file.
            if s.status == Status::Working && next == Status::Done {
                finished.push(s.id.clone());
            }
            s.status = next;
            s.quiet &= next == Status::Waiting;
            s.since = now_ms();
            changed = true;
        }
    }

    // A hook may have registered a session under an id the file has since replaced.
    let mut seen_ids = HashSet::new();
    let mut seen_pids = HashSet::new();
    let before = inner.sessions.len();
    inner.sessions.retain(|s| {
        seen_ids.insert(s.id.clone()) && s.terminal.claude_pid.is_none_or(|pid| seen_pids.insert(pid))
    });
    changed |= inner.sessions.len() != before;

    // Terminal-prompt popups follow the session's waiting state.
    let waiting: HashSet<String> =
        inner.sessions.iter().filter(|s| s.status == Status::Waiting).map(|s| s.id.clone()).collect();
    let before = inner.pending.len();
    inner.pending.retain(|p| p.view.kind != "prompt" || waiting.contains(&p.view.session_id));
    changed |= inner.pending.len() != before;
    let to_announce: Vec<(String, String)> = inner
        .sessions
        .iter()
        .filter(|s| s.status == Status::Waiting && !s.quiet && !inner.has_pending_for(&s.id))
        .map(|s| (s.id.clone(), s.cwd.clone()))
        .collect();
    drop(inner);

    for (session_id, cwd) in to_announce {
        let (tool_name, tool_input) = last_tool_call(&cwd, &session_id).unwrap_or_default();
        server::add_pending(app, "prompt", &session_id, &cwd, tool_name, tool_input, None);
    }
    for session_id in finished {
        server::announce_done(app, &session_id, None);
    }
    if changed {
        ui::refresh(app);
    }
}

/// Drops sessions that ended without a SessionEnd hook: the Claude process
/// was killed (closed tab, crash), or we never found its process and it has
/// gone quiet. Returns whether anything was removed.
fn prune_ended(inner: &mut Inner) -> bool {
    let now = now_ms();
    let ended: Vec<String> = inner
        .sessions
        .iter()
        .filter(|s| match s.terminal.claude_pid {
            Some(pid) => !focus::is_alive(pid),
            None => now.saturating_sub(s.last_seen) > UNMATCHED_TIMEOUT_MS && !inner.has_pending_for(&s.id),
        })
        .map(|s| s.id.clone())
        .collect();
    if ended.is_empty() {
        return false;
    }
    inner.sessions.retain(|s| !ended.contains(&s.id));
    inner.pending.retain(|p| !ended.contains(&p.view.session_id));
    true
}

/// Re-reads a session's file right before typing an answer, so a prompt the
/// user already answered in the terminal never gets a stray keystroke.
pub fn waiting_on_permission(pid: u32) -> bool {
    let Some(path) = dirs::home_dir().map(|h| h.join(".claude").join("sessions").join(format!("{pid}.json"))) else {
        return false;
    };
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<SessionFile>(&text).ok())
        .is_some_and(|f| f.status.as_deref() == Some("waiting") && f.waiting_for.as_deref() == Some("permission prompt"))
}

/// The most recent tool call in a session's transcript, which is what a
/// waiting permission prompt is about.
fn last_tool_call(cwd: &str, session_id: &str) -> Option<(String, Value)> {
    transcript_tail(cwd, session_id)?.lines().rev().find_map(|line| {
        let entry: Value = serde_json::from_str(line).ok()?;
        let content = entry.pointer("/message/content")?.as_array()?;
        let call = content.iter().rev().find(|c| c["type"] == "tool_use")?;
        Some((call["name"].as_str()?.to_string(), call["input"].clone()))
    })
}

/// The user's last prompt and Claude's last reply in a session's transcript.
pub fn last_exchange(cwd: &str, session_id: &str) -> (Option<String>, Option<String>) {
    transcript_tail(cwd, session_id).map(|tail| exchange_in(&tail)).unwrap_or_default()
}

fn exchange_in(tail: &str) -> (Option<String>, Option<String>) {
    let mut reply = None;
    for line in tail.lines().rev() {
        let Ok(entry) = serde_json::from_str::<Value>(line) else { continue };
        let content = entry.pointer("/message/content");
        match entry["type"].as_str() {
            Some("assistant") if reply.is_none() => reply = text_of(content),
            // Tool results and injected context are "user" entries too; real prompts are plain text.
            Some("user") if entry["isMeta"] != true => {
                if let Some(prompt) = text_of(content).filter(|t| !t.starts_with('<')) {
                    return (Some(prompt), reply);
                }
            }
            _ => {}
        }
    }
    (None, reply)
}

/// The text of a message's content, which is either a string or a list of blocks.
fn text_of(content: Option<&Value>) -> Option<String> {
    let text = match content? {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The end of a session's transcript, as text.
fn transcript_tail(cwd: &str, session_id: &str) -> Option<String> {
    // Claude Code stores transcripts under the cwd with every non-alphanumeric char as '-'.
    let project: String = cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let path = dirs::home_dir()?.join(".claude").join("projects").join(project).join(format!("{session_id}.jsonl"));
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(TRANSCRIPT_TAIL))).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    // The seek can land mid-character; the partial first line is skipped anyway.
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_last_prompt_and_reply() {
        let tail = [
            r#"{"type":"user","message":{"content":"old question"}}"#,
            r#"{"type":"user","message":{"content":"Add a dark mode toggle"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Edit","input":{}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","content":"ok"}]}}"#,
            r#"{"type":"user","isMeta":true,"message":{"content":"injected context"}}"#,
            r#"{"type":"user","message":{"content":"<command-name>/model</command-name>"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Added it to Settings."}]}}"#,
        ]
        .join("\n");
        assert_eq!(
            exchange_in(&tail),
            (Some("Add a dark mode toggle".into()), Some("Added it to Settings.".into()))
        );
    }

    #[test]
    fn handles_a_cut_off_first_line() {
        let tail = "e\":\"user\"}\n{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}";
        assert_eq!(exchange_in(tail), (Some("hi".into()), None));
    }
}
