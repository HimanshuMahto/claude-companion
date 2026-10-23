//! Tray icon, tray menu and the permission popup window.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tauri::{
    image::Image,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, PhysicalPosition,
};

use crate::discover;
use crate::focus;
use crate::hooks_install;
use crate::login;
use crate::state::{now_ms, AppState, Status};

const TRAY_ID: &str = "main";
const PROMPT_WINDOW: &str = "prompt";
const BLINK_MS: u64 = 600;
/// Check for dead Claude processes every this many blink ticks (~15s).
const PRUNE_EVERY: u32 = 25;
/// How long the green "done" ghost shows before settling back to idle.
const DONE_SECS: u64 = 45;

static HOOKS_INSTALLED: AtomicBool = AtomicBool::new(false);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);
static MENU_SIGNATURE: Mutex<String> = Mutex::new(String::new());

struct Icons {
    offline: Image<'static>,
    connected: Image<'static>,
    working: [Image<'static>; 2],
    waiting: [Image<'static>; 2],
    done: Image<'static>,
    /// Permission badges for 1..=9 pending requests, then "9+".
    attention: Vec<Image<'static>>,
}

fn icons() -> &'static Icons {
    static ICONS: OnceLock<Icons> = OnceLock::new();
    ICONS.get_or_init(|| {
        let load = |bytes: &'static [u8]| Image::from_bytes(bytes).expect("bundled tray icon");
        macro_rules! tray {
            ($name:literal) => {
                load(include_bytes!(concat!("../icons/tray/", $name, ".png")))
            };
        }
        Icons {
            offline: tray!("offline"),
            connected: tray!("connected"),
            working: [tray!("working-a"), tray!("working-b")],
            waiting: [tray!("waiting-a"), tray!("waiting-b")],
            done: tray!("done"),
            attention: vec![
                tray!("attention-1"),
                tray!("attention-2"),
                tray!("attention-3"),
                tray!("attention-4"),
                tray!("attention-5"),
                tray!("attention-6"),
                tray!("attention-7"),
                tray!("attention-8"),
                tray!("attention-9"),
                tray!("attention-9plus"),
            ],
        }
    })
}

pub fn setup(app: &AppHandle) -> tauri::Result<()> {
    if hooks_install::is_first_run() {
        *LAST_ERROR.lock().unwrap() = hooks_install::install().err();
    }
    HOOKS_INSTALLED.store(hooks_install::is_installed(), Ordering::Relaxed);
    #[cfg(target_os = "macos")]
    if let Some(win) = app.get_webview_window(PROMPT_WINDOW) {
        crate::panel::install(&win);
    }
    let (menu, _) = build_menu(app)?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icons().offline.clone())
        .icon_as_template(false)
        .tooltip("Claude Companion")
        .menu(&menu)
        .on_menu_event(on_menu_event)
        .build(app)?;

    let app = app.clone();
    tauri::async_runtime::spawn(async move { animate(app).await });
    Ok(())
}

/// Re-syncs the tray menu, tooltip and popup with the current state.
/// Safe to call from any thread.
pub fn refresh(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || refresh_now(&handle));
}

fn refresh_now(app: &AppHandle) {
    let (has_pending, tooltip) = {
        let state = app.state::<AppState>();
        let inner = state.0.lock().unwrap();
        let working = inner.sessions.iter().filter(|s| s.status == Status::Working).count();
        let tooltip = match (inner.sessions.len(), inner.pending.len()) {
            (0, _) => "Claude Companion: no sessions".to_string(),
            (n, 0) => format!("Claude Companion: {n} session(s), {working} working"),
            (n, p) => format!("Claude Companion: {n} session(s), {p} waiting on you"),
        };
        (!inner.pending.is_empty(), tooltip)
    };

    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(tooltip));
        if let Ok((menu, changed)) = build_menu(app) {
            if changed {
                let _ = tray.set_menu(Some(menu));
            }
        }
    }

    let _ = app.emit_to(PROMPT_WINDOW, "pending-changed", ());
    if has_pending {
        show_prompt(app);
    } else if let Some(win) = app.get_webview_window(PROMPT_WINDOW) {
        let _ = win.hide();
    }
}

pub fn show_prompt(app: &AppHandle) {
    let Some(win) = app.get_webview_window(PROMPT_WINDOW) else { return };
    if win.is_visible().unwrap_or(false) {
        return;
    }
    // Top-right corner of the screen the mouse is on, like a system notification.
    let monitor = win
        .cursor_position()
        .ok()
        .and_then(|p| win.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| win.primary_monitor().ok().flatten());
    if let Some(monitor) = monitor {
        let area = monitor.work_area();
        let width = win.outer_size().map(|s| s.width as i32).unwrap_or(0);
        let margin = (12.0 * monitor.scale_factor()) as i32;
        let x = area.position.x + area.size.width as i32 - width - margin;
        let y = area.position.y + margin;
        let _ = win.set_position(PhysicalPosition::new(x, y));
    }
    // Deliberately not focused: a stray Enter while typing must never approve anything.
    let _ = win.show();
    #[cfg(target_os = "macos")]
    crate::panel::order_front(&win);
}

/// Builds the tray menu; the bool says whether it differs from the last one built.
fn build_menu(app: &AppHandle) -> tauri::Result<(Menu<tauri::Wry>, bool)> {
    let state = app.state::<AppState>();
    let (sessions, pending) = {
        let inner = state.0.lock().unwrap();
        (inner.sessions.clone(), inner.pending.len())
    };
    let installed = HOOKS_INSTALLED.load(Ordering::Relaxed);
    let at_login = login::is_enabled(app);
    let error = LAST_ERROR.lock().unwrap().clone();

    let signature = format!(
        "{pending}|{installed}|{at_login}|{error:?}|{}",
        sessions
            .iter()
            .map(|s| format!("{}:{}:{:?}", s.id, s.display_name(), s.status))
            .collect::<Vec<_>>()
            .join(",")
    );
    let changed = {
        let mut last = MENU_SIGNATURE.lock().unwrap();
        let changed = *last != signature;
        *last = signature;
        changed
    };

    let menu = Menu::new(app)?;
    if sessions.is_empty() {
        menu.append(&MenuItem::with_id(app, "none", "No Claude sessions", false, None::<&str>)?)?;
    }
    for s in &sessions {
        let dot = match s.status {
            Status::Working => "●",
            Status::Waiting => "◆",
            Status::Idle => "○",
            Status::Done => "✓",
        };
        let label = format!("{dot}  {}: {}", s.display_name(), s.status.label());
        menu.append(&MenuItem::with_id(app, format!("focus:{}", s.id), label, true, None::<&str>)?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    if pending > 0 {
        let label = format!("Show {pending} pending request(s)");
        menu.append(&MenuItem::with_id(app, "show-pending", label, true, None::<&str>)?)?;
    }
    if installed {
        menu.append(&MenuItem::with_id(app, "uninstall", "Remove Claude Code hooks", true, None::<&str>)?)?;
    } else {
        menu.append(&MenuItem::with_id(app, "install", "Install Claude Code hooks", true, None::<&str>)?)?;
    }
    // Only the installed .app can register itself (see login::is_bundled).
    menu.append(&CheckMenuItem::with_id(app, "login", "Start at Login", login::is_bundled(), at_login, None::<&str>)?)?;
    if let Some(err) = error {
        menu.append(&MenuItem::with_id(app, "error", format!("⚠ {err}"), false, None::<&str>)?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(app, "quit", "Quit Claude Companion", true, None::<&str>)?)?;
    Ok((menu, changed))
}

fn on_menu_event(app: &AppHandle, event: MenuEvent) {
    let id = event.id().as_ref();
    match id {
        "quit" => app.exit(0),
        "show-pending" => show_prompt(app),
        "login" => {
            *LAST_ERROR.lock().unwrap() = login::toggle(app).err();
            refresh(app);
        }
        "install" | "uninstall" => {
            let result = if id == "install" { hooks_install::install() } else { hooks_install::uninstall() };
            HOOKS_INSTALLED.store(hooks_install::is_installed(), Ordering::Relaxed);
            *LAST_ERROR.lock().unwrap() = result.err();
            refresh(app);
        }
        _ => {
            if let Some(session_id) = id.strip_prefix("focus:") {
                let state = app.state::<AppState>();
                let has_popup = state.0.lock().unwrap().has_pending_for(session_id);
                if has_popup {
                    show_prompt(app);
                } else {
                    focus_session(app, session_id);
                }
            }
        }
    }
}

/// Types the user's popup answer into a terminal-hosted permission prompt,
/// falling back to just opening the terminal if anything looks off.
pub fn answer_in_terminal(app: &AppHandle, session_id: &str, answer: focus::Answer) {
    let state = app.state::<AppState>();
    let target = {
        let inner = state.0.lock().unwrap();
        inner.session(session_id).map(|s| (s.terminal.clone(), s.cwd.clone()))
    };
    let Some((terminal, cwd)) = target else { return };
    std::thread::spawn(move || {
        let result = match terminal.claude_pid {
            Some(pid) if discover::waiting_on_permission(pid) => focus::answer_prompt(&terminal, answer),
            _ => Err("the prompt is no longer waiting".to_string()),
        };
        if let Err(e) = result {
            eprintln!("claude-companion: couldn't answer in terminal ({e}); opening it instead");
            focus::focus_terminal(&terminal, &cwd);
        }
    });
}

pub fn focus_session(app: &AppHandle, session_id: &str) {
    let state = app.state::<AppState>();
    let target = {
        let inner = state.0.lock().unwrap();
        inner.session(session_id).map(|s| (s.terminal.clone(), s.cwd.clone()))
    };
    if let Some((terminal, cwd)) = target {
        // osascript can take a moment; keep it off the UI thread.
        std::thread::spawn(move || focus::focus_terminal(&terminal, &cwd));
    }
}

async fn animate(app: AppHandle) {
    let mut tick: u32 = 0;
    let mut last: usize = 0;
    let mut decayed = false;
    loop {
        tokio::time::sleep(Duration::from_millis(BLINK_MS)).await;
        tick = tick.wrapping_add(1);
        if tick % PRUNE_EVERY == 0 {
            prune_dead_sessions(&app);
        }

        let icon = {
            let state = app.state::<AppState>();
            let mut inner = state.0.lock().unwrap();
            let frame = (tick % 2) as usize;
            let i = icons();
            for s in inner.sessions.iter_mut() {
                if s.status == Status::Done && now_ms().saturating_sub(s.since) >= DONE_SECS * 1000 {
                    s.status = Status::Idle;
                    s.since = now_ms();
                    decayed = true;
                }
            }
            let any = |st: Status| inner.sessions.iter().any(|s| s.status == st);
            if !inner.pending.is_empty() {
                &i.attention[inner.pending.len().min(i.attention.len()) - 1]
            } else if any(Status::Waiting) {
                &i.waiting[frame]
            } else if any(Status::Working) {
                &i.working[frame]
            } else if any(Status::Done) {
                &i.done
            } else if !inner.sessions.is_empty() {
                &i.connected
            } else {
                &i.offline
            }
        };
        if std::mem::take(&mut decayed) {
            refresh(&app);
        }
        // Icons are statics, so their address identifies which one is showing.
        let key = icon as *const Image<'static> as usize;
        if last != key {
            last = key;
            if let Some(tray) = app.tray_by_id(TRAY_ID) {
                let _ = tray.set_icon(Some(icon.clone()));
            }
        }
    }
}

/// Drops sessions whose Claude process exited without a SessionEnd (killed terminal, crash).
fn prune_dead_sessions(app: &AppHandle) {
    let state = app.state::<AppState>();
    let pids: Vec<(String, u32)> = {
        let inner = state.0.lock().unwrap();
        inner.sessions.iter().filter_map(|s| Some((s.id.clone(), s.terminal.claude_pid?))).collect()
    };
    let dead: Vec<String> = pids.into_iter().filter(|(_, pid)| !focus::is_alive(*pid)).map(|(id, _)| id).collect();
    if dead.is_empty() {
        return;
    }
    {
        let mut inner = state.0.lock().unwrap();
        inner.sessions.retain(|s| !dead.contains(&s.id));
        inner.pending.retain(|p| !dead.contains(&p.view.session_id));
    }
    refresh(app);
}
