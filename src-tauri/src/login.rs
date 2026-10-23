//! Start at login, via a LaunchAgent (tauri-plugin-autostart).

use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

/// Only the installed .app should register itself; a dev build would point the
/// LaunchAgent at target/debug.
pub fn is_bundled() -> bool {
    std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
}

/// Turns start-at-login on the first time the installed app runs, and keeps
/// the registered path current if the app has been moved since.
pub fn setup(app: &AppHandle) {
    if !is_bundled() {
        return;
    }
    let Some(dir) = dirs::home_dir().map(|h| h.join(".claude-companion")) else { return };
    let marker = dir.join("login-configured");
    let launcher = app.autolaunch();
    if !marker.exists() || launcher.is_enabled().unwrap_or(false) {
        let _ = launcher.enable();
    }
    let _ = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(marker, ""));
}

pub fn is_enabled(app: &AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

pub fn toggle(app: &AppHandle) -> Result<(), String> {
    let launcher = app.autolaunch();
    let result = if is_enabled(app) { launcher.disable() } else { launcher.enable() };
    result.map_err(|e| format!("start at login: {e}"))
}
