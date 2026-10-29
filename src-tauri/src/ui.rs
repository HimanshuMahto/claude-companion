//! Tray icon, tray menu and the permission popup window.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tauri::{
    image::Image,
    menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager,
};

use crate::discover;
use crate::focus;
use crate::hooks_install;
use crate::login;
use crate::placement;
use crate::state::{now_ms, AppState, Status};

const TRAY_ID: &str = "main";
const PROMPT_WINDOW: &str = "prompt";
const BLINK_MS: u64 = 600;
/// How long the green "done" ghost shows before settling back to idle.
const DONE_SECS: u64 = 45;
/// Check that a wanted popup is really on screen every this many blink ticks (~1.8s).
const WATCH_EVERY: u32 = 3;
/// Showing it again this many times in a row without luck means relaunching.
const RELAUNCH_AFTER: u32 = 3;
/// Never relaunch more often than this, even across relaunches.
const RELAUNCH_GAP_MS: u64 = 10 * 60 * 1000;

/// Watchdog checks in a row that found the popup wanted but off screen.
static OFF_SCREEN: AtomicU32 = AtomicU32::new(0);

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
    placement::load();
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
        let tooltip = match (inner.sessions.len(), inner.attention()) {
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
    // Already up: don't move it, but do make sure it's really in front.
    let appearing = !win.is_visible().unwrap_or(false);
    if appearing {
        placement::place(&win);
    }
    // Never key or focused: keys typed elsewhere must never reach the buttons.
    #[cfg(target_os = "macos")]
    crate::panel::order_front(&win);
    #[cfg(not(target_os = "macos"))]
    let _ = win.show();
    if appearing {
        // Restarts the click delay on the buttons.
        let _ = app.emit_to(PROMPT_WINDOW, "popup-shown", ());
    }
}

/// After the Mac wakes from sleep, macOS can stop putting the popup on
/// screen even though it's shown, and only a fresh app process brings it back.
/// When a popup is wanted but not visible: show it again, and if that keeps
/// failing, relaunch the app. Held requests fall back to Claude Code's own
/// prompt, and the relaunched app pops those up again from the session files.
#[cfg(target_os = "macos")]
fn watch_popup(app: &AppHandle) {
    let wanted = !app.state::<AppState>().0.lock().unwrap().pending.is_empty();
    if !wanted {
        OFF_SCREEN.store(0, Ordering::Relaxed);
        return;
    }
    // Nothing can be on screen behind the lock screen, screen saver or a sleeping display.
    let front = focus::frontmost_bundle().unwrap_or_default();
    if front == "com.apple.loginwindow" || front == "com.apple.ScreenSaver.Engine" || crate::panel::display_asleep() {
        return;
    }
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(win) = handle.get_webview_window(PROMPT_WINDOW) else { return };
        if crate::panel::is_on_screen(&win) {
            OFF_SCREEN.store(0, Ordering::Relaxed);
            return;
        }
        let misses = OFF_SCREEN.fetch_add(1, Ordering::Relaxed) + 1;
        let relaunch = misses >= RELAUNCH_AFTER && login::is_bundled() && crate::panel::relaunch_allowed(RELAUNCH_GAP_MS);
        crate::panel::log_recovery(&format!(
            "popup off screen (check {misses}): {}; {}",
            crate::panel::debug_state(&win),
            if relaunch { "relaunching" } else { "showing it again" }
        ));
        if relaunch {
            relaunch_app(&handle);
        } else {
            let _ = win.hide();
            show_prompt(&handle);
        }
    });
}

/// Starts a fresh copy of the app once this one has quit.
#[cfg(target_os = "macos")]
fn relaunch_app(app: &AppHandle) {
    // .../Claude Companion.app/Contents/MacOS/claude-companion -> .../Claude Companion.app
    let Some(bundle) = std::env::current_exe().ok().and_then(|exe| exe.ancestors().nth(3).map(|p| p.to_path_buf())) else {
        return;
    };
    // A second copy started now would exit at once (single instance), so wait for this one to go.
    let spawned = std::process::Command::new("/bin/sh").args(["-c", "sleep 1; open \"$0\""]).arg(&bundle).spawn();
    if spawned.is_ok() {
        app.exit(0);
    }
}

/// The popup window's real state, for the /sessions debug endpoint.
pub fn popup_debug(app: &AppHandle) -> serde_json::Value {
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let state = handle.get_webview_window(PROMPT_WINDOW).map(|win| {
            #[cfg(target_os = "macos")]
            let mut state = crate::panel::debug_state(&win);
            #[cfg(not(target_os = "macos"))]
            let mut state = serde_json::json!({});
            state["tauri_visible"] = win.is_visible().unwrap_or(false).into();
            state
        });
        let _ = tx.send(state);
    });
    let window = rx.recv_timeout(Duration::from_secs(1)).ok().flatten().unwrap_or_default();
    serde_json::json!({
        "window": window,
        "off_screen_checks": OFF_SCREEN.load(Ordering::Relaxed),
    })
}

/// Builds the tray menu; the bool says whether it differs from the last one built.
fn build_menu(app: &AppHandle) -> tauri::Result<(Menu<tauri::Wry>, bool)> {
    let state = app.state::<AppState>();
    let (sessions, pending) = {
        let inner = state.0.lock().unwrap();
        (inner.sessions.clone(), inner.attention())
    };
    let installed = HOOKS_INSTALLED.load(Ordering::Relaxed);
    let at_login = login::is_enabled(app);
    let moved = placement::has_user_position();
    let error = LAST_ERROR.lock().unwrap().clone();

    let items: Vec<(String, String)> = sessions
        .iter()
        .map(|s| {
            let dot = match s.status {
                Status::Working => "●",
                Status::Waiting => "◆",
                Status::Idle => "○",
                Status::Done => "✓",
            };
            // Naming the app helps find sessions left running in an IDE tab.
            let app = focus::app_name(&s.terminal);
            let place = if app.is_empty() { String::new() } else { format!(" · {app}") };
            (s.id.clone(), format!("{dot}  {}{place}: {}", s.display_name(), s.status.label()))
        })
        .collect();
    let signature = format!(
        "{pending}|{installed}|{at_login}|{moved}|{error:?}|{}",
        items.iter().map(|(id, label)| format!("{id}:{label}")).collect::<Vec<_>>().join(",")
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
    for (id, label) in items {
        menu.append(&MenuItem::with_id(app, format!("focus:{id}"), label, true, None::<&str>)?)?;
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
    if moved {
        menu.append(&MenuItem::with_id(app, "reset-position", "Reset popup position", true, None::<&str>)?)?;
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
        "reset-position" => {
            placement::reset(app.get_webview_window(PROMPT_WINDOW).as_ref());
            refresh(app);
        }
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
        #[cfg(target_os = "macos")]
        if tick % WATCH_EVERY == 0 {
            watch_popup(&app);
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
            let attention = inner.attention();
            if attention > 0 {
                &i.attention[attention.min(i.attention.len()) - 1]
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
