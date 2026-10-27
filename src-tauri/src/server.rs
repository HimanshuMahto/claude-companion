//! Local HTTP endpoint that the Claude Code hook script posts every event to.
//!
//! Lifecycle events update session status and return immediately. A
//! `PermissionRequest` holds the request open until the user answers in the
//! popup, then replies with the decision JSON Claude Code expects. An empty
//! reply means "no decision", so Claude Code shows its normal terminal prompt.

use std::time::Duration;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio::sync::oneshot;

use crate::discover;
use crate::focus;
use crate::state::{now_ms, project_name, AppState, Decision, Pending, PendingView, Session, Status, Terminal};
use crate::ui;

pub const PORT: u16 = 47823;
/// How long the popup waits before handing the prompt back to the terminal.
/// Must stay below the hook's curl --max-time and Claude Code's hook timeout.
pub const PROMPT_TIMEOUT_SECS: u64 = 540;
/// How long a "task finished" notice stays up.
pub const DONE_TIMEOUT_SECS: u64 = 12;

pub async fn serve(app: AppHandle) {
    let router = Router::new()
        .route("/hook", post(hook))
        .route("/sessions", get(sessions))
        .with_state(app);
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", PORT)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("claude-companion: cannot bind 127.0.0.1:{PORT}: {e}");
            return;
        }
    };
    if let Err(e) = axum::serve(listener, router).await {
        eprintln!("claude-companion: server stopped: {e}");
    }
}

/// Debug view of what the app currently knows: `curl 127.0.0.1:47823/sessions`
async fn sessions(State(app): State<AppHandle>) -> Json<Value> {
    let state = app.state::<AppState>();
    let inner = state.0.lock().unwrap();
    Json(json!({
        "sessions": inner.sessions.iter().map(|s| json!({
            "id": s.id,
            "cwd": s.cwd,
            "status": s.status,
            "claude_pid": s.terminal.claude_pid,
            "tty": s.terminal.tty,
            "term_program": s.terminal.term_program,
            "bundle_id": s.terminal.bundle_id,
        })).collect::<Vec<_>>(),
        "pending": inner.pending.iter().map(|p| &p.view).collect::<Vec<_>>(),
    }))
}

type Reply = Response;

/// No output: Claude Code carries on as if the hook weren't there.
fn empty() -> Reply {
    StatusCode::OK.into_response()
}

async fn hook(State(app): State<AppHandle>, headers: HeaderMap, body: String) -> Result<Reply, StatusCode> {
    // Browsers can't send this custom header cross-origin without a preflight we
    // never answer, so web pages can't spam popups through this port.
    let pid = header(&headers, "x-cc-pid").ok_or(StatusCode::FORBIDDEN)?;
    let event: Value = serde_json::from_str(&body).map_err(|_| StatusCode::BAD_REQUEST)?;

    let name = str_field(&event, "hook_event_name");
    let session_id = str_field(&event, "session_id");
    if session_id.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let cwd = str_field(&event, "cwd");

    if name == "SessionEnd" {
        let state = app.state::<AppState>();
        let mut inner = state.0.lock().unwrap();
        inner.sessions.retain(|s| s.id != session_id);
        inner.pending.retain(|p| p.view.session_id != session_id);
        drop(inner);
        ui::refresh(&app);
        return Ok(empty());
    }

    track_session(&app, &session_id, &cwd, &headers, pid);
    // Notification fires when the terminal shows its own prompt, so it doesn't count.
    if name != "PermissionRequest" && name != "Notification" {
        clear_terminal_popups(&app, &session_id);
    }

    let status = match name.as_str() {
        "SessionStart" => Some(Status::Idle),
        "Stop" => Some(Status::Done),
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => Some(Status::Working),
        "Notification" => match str_field(&event, "notification_type").as_str() {
            "idle_prompt" => Some(Status::Idle),
            "permission_prompt" | "elicitation_dialog" => Some(Status::Waiting),
            _ => None,
        },
        "PermissionRequest" => Some(Status::Waiting),
        _ => None,
    };
    if name == "UserPromptSubmit" {
        let prompt = str_field(&event, "prompt");
        let state = app.state::<AppState>();
        let mut inner = state.0.lock().unwrap();
        if let Some(s) = inner.session_mut(&session_id) {
            s.last_prompt = (!prompt.is_empty()).then_some(prompt);
        }
    }
    if let Some(status) = status {
        let was_working = set_status(&app, &session_id, status) == Some(Status::Working);
        if status == Status::Done && was_working {
            let reply = str_field(&event, "last_assistant_message");
            announce_done(&app, &session_id, (!reply.is_empty()).then_some(reply));
        }
    }

    if name != "PermissionRequest" {
        return Ok(empty());
    }

    let tool_name = str_field(&event, "tool_name");
    let tool_input = event.get("tool_input").cloned().unwrap_or(Value::Null);

    // Questions need the terminal's own picker, so don't hold Claude up: show
    // a heads-up popup and let Claude Code prompt in the terminal right away.
    if tool_name == "AskUserQuestion" {
        add_pending(&app, "question", &session_id, &cwd, tool_name, tool_input, None);
        return Ok(empty());
    }

    let (tx, rx) = oneshot::channel();
    let id = add_pending(&app, "permission", &session_id, &cwd, tool_name, tool_input, Some(tx));
    // If Claude Code gives up on the hook (user answered in the terminal, Ctrl-C),
    // the connection drops and this future is dropped; the guard clears the popup.
    let _guard = PendingGuard { app: app.clone(), id };

    let decision = tokio::time::timeout(Duration::from_secs(PROMPT_TIMEOUT_SECS), rx)
        .await
        .ok()
        .and_then(Result::ok);

    match decision {
        Some(Decision::Allow) => {
            set_status(&app, &session_id, Status::Working);
            Ok(Json(permission_output(json!({ "behavior": "allow" }))).into_response())
        }
        Some(Decision::Deny) => {
            set_status(&app, &session_id, Status::Idle);
            Ok(Json(permission_output(json!({
                "behavior": "deny",
                "message": "The user denied this request from Claude Companion.",
                "interrupt": true
            })))
            .into_response())
        }
        // Go to terminal, or timed out: no decision, so the terminal prompt appears.
        Some(Decision::Terminal) | None => Ok(empty()),
    }
}

fn permission_output(decision: Value) -> Value {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": decision
        }
    })
}

struct PendingGuard {
    app: AppHandle,
    id: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        let state = self.app.state::<AppState>();
        let mut inner = state.0.lock().unwrap();
        let removed = inner.take_pending(self.id);
        // Timed out or cancelled: the prompt is in the terminal now; don't re-pop it.
        if let Some(p) = &removed {
            inner.set_quiet(&p.view.session_id);
        }
        drop(inner);
        if removed.is_some() {
            ui::refresh(&self.app);
        }
    }
}

/// Shows a "task finished" notice for a session that just finished a turn.
/// `reply` is Claude's last message when the hook provided it.
pub fn announce_done(app: &AppHandle, session_id: &str, reply: Option<String>) {
    let state = app.state::<AppState>();
    let session = {
        let mut inner = state.0.lock().unwrap();
        // Something more important is already up for this session.
        if inner.pending.iter().any(|p| p.view.session_id == session_id && p.view.kind != "done") {
            return;
        }
        inner.pending.retain(|p| p.view.session_id != session_id);
        inner.session(session_id).map(|s| (s.cwd.clone(), s.last_prompt.clone()))
    };
    let Some((cwd, prompt)) = session else { return };
    // Sessions that predate the hooks only have their transcript to go on.
    let (prompt, reply) = match (prompt, reply) {
        (Some(prompt), Some(reply)) => (Some(prompt), Some(reply)),
        (prompt, reply) => {
            let (p, r) = discover::last_exchange(&cwd, session_id);
            (prompt.or(p), reply.or(r))
        }
    };
    let input = json!({ "prompt": prompt, "reply": reply });
    let id = add_pending(app, "done", session_id, &cwd, String::new(), input, None);

    let app = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(DONE_TIMEOUT_SECS));
        let state = app.state::<AppState>();
        let removed = state.0.lock().unwrap().take_pending(id).is_some();
        if removed {
            ui::refresh(&app);
        }
    });
}

pub fn add_pending(
    app: &AppHandle,
    kind: &'static str,
    session_id: &str,
    cwd: &str,
    tool_name: String,
    tool_input: Value,
    reply: Option<oneshot::Sender<Decision>>,
) -> u64 {
    let state = app.state::<AppState>();
    let mut inner = state.0.lock().unwrap();
    let id = inner.next_id();
    let (session_name, terminal_app, can_answer) = inner
        .session(session_id)
        .map(|s| {
            let at_permission_menu = s.waiting_for.as_deref() == Some("permission prompt");
            let can_answer = kind == "prompt" && at_permission_menu && focus::can_type_answers(&s.terminal);
            (s.name.clone(), focus::app_name(&s.terminal), can_answer)
        })
        .unwrap_or_default();
    if reply.is_some() {
        // A real request supersedes the "go look at the terminal" popup.
        inner.pending.retain(|p| !(p.view.kind == "prompt" && p.view.session_id == session_id));
    }
    inner.pending.push(Pending {
        view: PendingView {
            id,
            kind,
            session_id: session_id.to_string(),
            project: project_name(cwd),
            session_name,
            terminal_app,
            cwd: cwd.to_string(),
            tool_name,
            tool_input,
            created_at: now_ms(),
            timeout_secs: if kind == "done" { DONE_TIMEOUT_SECS } else { PROMPT_TIMEOUT_SECS },
            can_answer,
        },
        reply,
    });
    drop(inner);
    ui::refresh(app);
    id
}

fn track_session(app: &AppHandle, session_id: &str, cwd: &str, headers: &HeaderMap, pid: String) {
    let state = app.state::<AppState>();
    let needs_resolve = {
        let mut inner = state.0.lock().unwrap();
        match inner.session_mut(session_id) {
            // Keep the launch directory; the hook's cwd follows wherever Claude cd'd to.
            Some(s) => {
                s.last_seen = now_ms();
                s.terminal.claude_pid.is_none()
            }
            None => {
                inner.sessions.push(Session {
                    id: session_id.to_string(),
                    cwd: cwd.to_string(),
                    name: None,
                    quiet: false,
                    waiting_for: None,
                    last_prompt: None,
                    last_seen: now_ms(),
                    status: Status::Idle,
                    since: now_ms(),
                    terminal: Terminal {
                        term_program: header(headers, "x-cc-term").unwrap_or_default(),
                        bundle_id: header(headers, "x-cc-bundle").unwrap_or_default(),
                        iterm_session: header(headers, "x-cc-iterm").unwrap_or_default(),
                        ..Default::default()
                    },
                });
                true
            }
        }
    };
    if !needs_resolve {
        return;
    }
    let Ok(pid) = pid.trim().parse::<u32>() else { return };
    if let Some((claude_pid, tty)) = focus::resolve_claude_process(pid) {
        let mut inner = state.0.lock().unwrap();
        if let Some(s) = inner.session_mut(session_id) {
            s.terminal.claude_pid = Some(claude_pid);
            s.terminal.tty = tty;
        }
    }
}

/// Any further activity means whatever the terminal was asking got answered.
fn clear_terminal_popups(app: &AppHandle, session_id: &str) {
    let state = app.state::<AppState>();
    let removed = {
        let mut inner = state.0.lock().unwrap();
        let before = inner.pending.len();
        inner.pending.retain(|p| !(p.view.kind != "permission" && p.view.session_id == session_id));
        inner.pending.len() != before
    };
    if removed {
        ui::refresh(app);
    }
}

/// Returns the status the session had before, if it changed.
fn set_status(app: &AppHandle, session_id: &str, status: Status) -> Option<Status> {
    let state = app.state::<AppState>();
    let previous = {
        let mut inner = state.0.lock().unwrap();
        match inner.session_mut(session_id) {
            Some(s) if s.status != status => {
                let previous = s.status;
                s.status = status;
                s.quiet &= status == Status::Waiting;
                s.since = now_ms();
                Some(previous)
            }
            _ => None,
        }
    };
    if previous.is_some() {
        ui::refresh(app);
    }
    previous
}

fn header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}
