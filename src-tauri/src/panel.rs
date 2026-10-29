//! macOS: turns the popup into a floating, non-activating panel (like
//! Spotlight) so it shows on every Space, above full-screen apps, without
//! stealing focus from whatever the user is typing in.

use objc2::{class, msg_send, runtime::AnyObject};
use tauri::WebviewWindow;
use window_vibrancy::{apply_vibrancy, NSVisualEffectMaterial, NSVisualEffectState};

const NONACTIVATING_PANEL: usize = 1 << 7;
const CAN_JOIN_ALL_SPACES: usize = 1 << 0;
const STATIONARY: usize = 1 << 4;
const FULL_SCREEN_AUXILIARY: usize = 1 << 8;
const STATUS_WINDOW_LEVEL: isize = 25;
/// NSWindowOcclusionStateVisible: some part of the window is actually on screen.
const OCCLUSION_VISIBLE: usize = 1 << 1;
pub const CORNER_RADIUS: f64 = 16.0;

fn ns_window(win: &WebviewWindow) -> Option<*mut AnyObject> {
    win.ns_window().ok().map(|p| p as *mut AnyObject)
}

/// One-time setup; call on the main thread before the window is first shown.
pub fn install(win: &WebviewWindow) {
    let Some(w) = ns_window(win) else { return };
    // SAFETY: `w` is the live NSWindow and we're on the main thread. Swapping the
    // class to NSPanel (an NSWindow subclass) is the same trick tauri-nspanel uses;
    // only a panel may float over another app's full-screen Space.
    unsafe {
        AnyObject::set_class(&*w, class!(NSPanel));
        configure(w);
    }
    let _ = apply_vibrancy(
        win,
        NSVisualEffectMaterial::HudWindow,
        Some(NSVisualEffectState::Active),
        Some(CORNER_RADIUS),
    );
}

/// The floating, every-Space panel behaviour. Re-applied before every show,
/// since we can't rule out macOS dropping it across sleep or display changes.
///
/// SAFETY: `w` must be the live panel and this must run on the main thread.
unsafe fn configure(w: *mut AnyObject) {
    let mask: usize = msg_send![w, styleMask];
    let _: () = msg_send![w, setStyleMask: mask | NONACTIVATING_PANEL];
    let _: () = msg_send![w, setFloatingPanel: true];
    let _: () = msg_send![w, setHidesOnDeactivate: false];
    let _: () = msg_send![w, setBecomesKeyOnlyIfNeeded: true];
    let _: () = msg_send![w, setCollectionBehavior: CAN_JOIN_ALL_SPACES | STATIONARY | FULL_SCREEN_AUXILIARY];
    let _: () = msg_send![w, setLevel: STATUS_WINDOW_LEVEL];
}

/// Shows the panel on the current Space even though our app isn't active.
pub fn order_front(win: &WebviewWindow) {
    let Some(w) = ns_window(win) else { return };
    // SAFETY: as above.
    unsafe {
        configure(w);
        let _: () = msg_send![w, orderFrontRegardless];
    }
}

/// Whether any part of the panel is really on screen, as opposed to
/// AppKit merely believing it's shown. Main thread only.
pub fn is_on_screen(win: &WebviewWindow) -> bool {
    let Some(w) = ns_window(win) else { return false };
    // SAFETY: as above.
    unsafe {
        let visible: bool = msg_send![w, isVisible];
        let occlusion: usize = msg_send![w, occlusionState];
        visible && occlusion & OCCLUSION_VISIBLE != 0
    }
}

/// What AppKit says about the panel, for the /sessions debug endpoint. Main thread only.
pub fn debug_state(win: &WebviewWindow) -> serde_json::Value {
    let Some(w) = ns_window(win) else { return serde_json::Value::Null };
    // SAFETY: as above.
    unsafe {
        let visible: bool = msg_send![w, isVisible];
        let key: bool = msg_send![w, isKeyWindow];
        let on_active_space: bool = msg_send![w, isOnActiveSpace];
        let occlusion: usize = msg_send![w, occlusionState];
        let level: isize = msg_send![w, level];
        let behavior: usize = msg_send![w, collectionBehavior];
        serde_json::json!({
            "visible": visible,
            "key": key,
            "on_active_space": on_active_space,
            "occlusion_visible": occlusion & OCCLUSION_VISIBLE != 0,
            "level": level,
            "collection_behavior": behavior,
        })
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGDisplayIsAsleep(display: u32) -> u32;
}

pub fn display_asleep() -> bool {
    // SAFETY: plain CoreGraphics queries with no pointers involved.
    unsafe { CGDisplayIsAsleep(CGMainDisplayID()) != 0 }
}

fn companion_file(name: &str) -> Option<std::path::PathBuf> {
    Some(dirs::home_dir()?.join(".claude-companion").join(name))
}

/// Whether the last relaunch was more than `gap_ms` ago; if so, records this one.
/// Kept on disk so a relaunched app can't relaunch again straight away.
pub fn relaunch_allowed(gap_ms: u64) -> bool {
    let Some(file) = companion_file("last-relaunch") else { return false };
    let last: u64 = std::fs::read_to_string(&file).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0);
    let now = crate::state::now_ms();
    if now.saturating_sub(last) < gap_ms {
        return false;
    }
    std::fs::write(file, now.to_string()).is_ok()
}

/// Appends to ~/.claude-companion/popup.log, so a popup that went missing can be diagnosed.
pub fn log_recovery(line: &str) {
    use std::io::Write;
    let Some(file) = companion_file("popup.log") else { return };
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(file) {
        let _ = writeln!(f, "{} {line}", crate::state::now_ms());
    }
}
