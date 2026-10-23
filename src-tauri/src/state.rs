use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use tokio::sync::oneshot;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Idle,
    Working,
    Waiting,
    /// Just finished a turn; decays back to Idle after a short while.
    Done,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Idle => "Idle",
            Status::Working => "Working",
            Status::Waiting => "Waiting for approval",
            Status::Done => "Done",
        }
    }
}

/// Where a session's terminal lives, captured from the hook process environment.
#[derive(Clone, Default, Debug)]
pub struct Terminal {
    pub claude_pid: Option<u32>,
    pub tty: Option<String>,
    pub term_program: String,
    pub bundle_id: String,
    pub iterm_session: String,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    pub cwd: String,
    /// Claude Code's session title, when known.
    pub name: Option<String>,
    pub status: Status,
    /// When `status` last changed (epoch ms).
    pub since: u64,
    /// The user already saw (and sent to the terminal or dismissed) the popup
    /// for this wait; don't pop it again until the session stops waiting.
    pub quiet: bool,
    /// What a waiting session waits for, per Claude Code ("permission prompt", ...).
    pub waiting_for: Option<String>,
    pub terminal: Terminal,
}

impl Session {
    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| project_name(&self.cwd))
    }
}

/// What the user picked in the popup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
    Terminal,
}

#[derive(Clone, Serialize)]
pub struct PendingView {
    pub id: u64,
    /// "permission": a hooked request, held until Allow/Deny/Go to terminal.
    /// "question": AskUserQuestion, answered in the terminal.
    /// "prompt": a terminal prompt we can only point at (no hook involved).
    pub kind: &'static str,
    pub session_id: String,
    pub project: String,
    /// Claude Code's session title, if it has one.
    pub session_name: Option<String>,
    /// Where the session runs, e.g. "WebStorm" or "Terminal".
    pub terminal_app: String,
    pub cwd: String,
    pub tool_name: String,
    pub tool_input: Value,
    pub created_at: u64,
    pub timeout_secs: u64,
    /// For "prompt": we can answer by typing into the terminal (Terminal.app, iTerm).
    pub can_answer: bool,
}

pub struct Pending {
    pub view: PendingView,
    pub reply: Option<oneshot::Sender<Decision>>,
}

#[derive(Default)]
pub struct Inner {
    pub sessions: Vec<Session>,
    pub pending: Vec<Pending>,
    next_id: u64,
}

impl Inner {
    pub fn session_mut(&mut self, id: &str) -> Option<&mut Session> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    pub fn session(&self, id: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn has_pending_for(&self, session_id: &str) -> bool {
        self.pending.iter().any(|p| p.view.session_id == session_id)
    }

    pub fn set_quiet(&mut self, session_id: &str) {
        if let Some(s) = self.session_mut(session_id) {
            s.quiet = true;
        }
    }

    /// Removes a pending entry, returning its reply channel if it still had one.
    pub fn take_pending(&mut self, id: u64) -> Option<Pending> {
        let idx = self.pending.iter().position(|p| p.view.id == id)?;
        Some(self.pending.remove(idx))
    }
}

#[derive(Default)]
pub struct AppState(pub Mutex<Inner>);

pub fn project_name(cwd: &str) -> String {
    std::path::Path::new(cwd)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.to_string())
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
