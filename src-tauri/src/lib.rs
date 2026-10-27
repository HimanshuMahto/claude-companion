//! Claude Companion: a menu bar app that shows what your Claude Code sessions
//! are doing and lets you answer their permission prompts from a popup.

mod discover;
mod focus;
mod hooks_install;
mod login;
#[cfg(target_os = "macos")]
mod panel;
mod placement;
mod server;
mod state;
mod ui;

use tauri::{AppHandle, State, WebviewWindow};

use state::{AppState, Decision, PendingView};

#[tauri::command]
fn get_pending(state: State<AppState>) -> Vec<PendingView> {
    state.0.lock().unwrap().pending.iter().map(|p| p.view.clone()).collect()
}

/// action: "allow" | "deny" | "terminal" | "dismiss"
#[tauri::command]
fn respond(app: AppHandle, state: State<AppState>, id: u64, action: String) {
    let decision = match action.as_str() {
        "allow" => Some(Decision::Allow),
        "deny" => Some(Decision::Deny),
        "terminal" => Some(Decision::Terminal),
        _ => None,
    };
    let pending = {
        let mut inner = state.0.lock().unwrap();
        let pending = inner.take_pending(id);
        // Handled: don't pop this same wait up again while the session catches up.
        if let Some(p) = pending.as_ref().filter(|p| p.view.kind != "done") {
            inner.set_quiet(&p.view.session_id);
        }
        pending
    };
    let Some(pending) = pending else { return };
    let session_id = &pending.view.session_id;
    match (pending.reply, decision) {
        (Some(reply), Some(decision)) => {
            let _ = reply.send(decision);
        }
        // No hook to reply through: type the answer into the terminal instead.
        (None, Some(Decision::Allow)) if pending.view.can_answer => {
            ui::answer_in_terminal(&app, session_id, focus::Answer::Allow)
        }
        (None, Some(Decision::Deny)) if pending.view.can_answer => {
            ui::answer_in_terminal(&app, session_id, focus::Answer::Deny)
        }
        _ => {}
    }
    if decision == Some(Decision::Terminal) {
        ui::focus_session(&app, session_id);
    }
    ui::refresh(&app);
}

/// The popup was dragged; remember where it was dropped.
#[tauri::command]
fn popup_moved(app: AppHandle, window: WebviewWindow) {
    if placement::remember(&window) {
        ui::refresh(&app);
    }
}

pub fn run() {
    tauri::Builder::default()
        // Must be registered first. A second launch (login + opening it by hand) just exits.
        .plugin(tauri_plugin_single_instance::init(|_app, _args, _cwd| {}))
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![get_pending, respond, popup_moved])
        .setup(|app| {
            // Menu bar only: no Dock icon, no app switcher entry.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let handle = app.handle().clone();
            login::setup(&handle);
            ui::setup(&handle)?;
            discover::spawn(handle.clone());
            tauri::async_runtime::spawn(server::serve(handle));
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Claude Companion")
        .run(|_app, event| {
            // Hiding the popup must never quit the app; only the tray's Quit does.
            if let tauri::RunEvent::ExitRequested { api, code, .. } = event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
        });
}
