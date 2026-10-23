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
        let mask: usize = msg_send![w, styleMask];
        let _: () = msg_send![w, setStyleMask: mask | NONACTIVATING_PANEL];
        let _: () = msg_send![w, setFloatingPanel: true];
        let _: () = msg_send![w, setHidesOnDeactivate: false];
        let _: () = msg_send![w, setBecomesKeyOnlyIfNeeded: true];
        let _: () = msg_send![w, setCollectionBehavior: CAN_JOIN_ALL_SPACES | STATIONARY | FULL_SCREEN_AUXILIARY];
        let _: () = msg_send![w, setLevel: STATUS_WINDOW_LEVEL];
    }
    let _ = apply_vibrancy(
        win,
        NSVisualEffectMaterial::HudWindow,
        Some(NSVisualEffectState::Active),
        Some(CORNER_RADIUS),
    );
}

/// Shows the panel on the current Space even though our app isn't active.
pub fn order_front(win: &WebviewWindow) {
    let Some(w) = ns_window(win) else { return };
    // SAFETY: as above.
    unsafe {
        let _: () = msg_send![w, orderFrontRegardless];
    }
}
