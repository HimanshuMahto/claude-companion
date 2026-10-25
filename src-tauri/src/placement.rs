//! Where the popup appears: top-right of the screen under the mouse, unless
//! the user has dragged it somewhere, in which case it reopens there (also
//! across restarts) for as long as that spot is still on a connected screen.

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use tauri::{PhysicalPosition, WebviewWindow};

/// The user's chosen spot (physical px), if they've moved the popup.
static USER_POSITION: Mutex<Option<(i32, i32)>> = Mutex::new(None);
/// Where we last put the window ourselves, to tell a real drag from a click.
static PLACED_AT: Mutex<Option<(i32, i32)>> = Mutex::new(None);

fn file() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".claude-companion").join("popup-position"))
}

pub fn load() {
    let saved = file().and_then(|f| fs::read_to_string(f).ok());
    *USER_POSITION.lock().unwrap() = saved.as_deref().and_then(parse_position);
}

/// Parses the saved "x y" line; anything else is ignored.
fn parse_position(text: &str) -> Option<(i32, i32)> {
    let (x, y) = text.trim().split_once(' ')?;
    Some((x.parse().ok()?, y.parse().ok()?))
}

pub fn has_user_position() -> bool {
    USER_POSITION.lock().unwrap().is_some()
}

/// Moves the window to its spot before it's shown.
pub fn place(win: &WebviewWindow) {
    let user = *USER_POSITION.lock().unwrap();
    let pos = user.filter(|&(x, y)| on_a_screen(win, x, y)).or_else(|| default_position(win));
    if let Some((x, y)) = pos {
        let _ = win.set_position(PhysicalPosition::new(x, y));
        *PLACED_AT.lock().unwrap() = Some((x, y));
    }
}

/// Called after the user finishes dragging the popup.
pub fn remember(win: &WebviewWindow) -> bool {
    let Ok(pos) = win.outer_position() else { return false };
    let pos = (pos.x, pos.y);
    if *PLACED_AT.lock().unwrap() == Some(pos) {
        return false; // a click, not a drag
    }
    *PLACED_AT.lock().unwrap() = Some(pos);
    *USER_POSITION.lock().unwrap() = Some(pos);
    if let Some(f) = file() {
        let _ = fs::write(f, format!("{} {}\n", pos.0, pos.1));
    }
    true
}

/// Forgets the user's spot; moves the popup back if it's open.
pub fn reset(win: Option<&WebviewWindow>) {
    *USER_POSITION.lock().unwrap() = None;
    if let Some(f) = file() {
        let _ = fs::remove_file(f);
    }
    if let Some(win) = win.filter(|w| w.is_visible().unwrap_or(false)) {
        place(win);
    }
}

/// Top-right corner of the screen the mouse is on, like a system notification.
fn default_position(win: &WebviewWindow) -> Option<(i32, i32)> {
    let monitor = win
        .cursor_position()
        .ok()
        .and_then(|p| win.monitor_from_point(p.x, p.y).ok().flatten())
        .or_else(|| win.primary_monitor().ok().flatten())?;
    let area = monitor.work_area();
    let width = win.outer_size().map(|s| s.width as i32).unwrap_or(0);
    let margin = (12.0 * monitor.scale_factor()) as i32;
    Some((area.position.x + area.size.width as i32 - width - margin, area.position.y + margin))
}

/// Whether the popup's top-left corner would land on a connected screen.
fn on_a_screen(win: &WebviewWindow, x: i32, y: i32) -> bool {
    win.available_monitors().unwrap_or_default().iter().any(|m| {
        let (p, s) = (m.position(), m.size());
        x >= p.x && y >= p.y && x < p.x + s.width as i32 && y < p.y + s.height as i32
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_saved_position() {
        assert_eq!(parse_position("1840 64\n"), Some((1840, 64)));
        // Screens left of or above the main one have negative coordinates.
        assert_eq!(parse_position("-1200 -30"), Some((-1200, -30)));
    }

    #[test]
    fn ignores_garbage() {
        assert_eq!(parse_position(""), None);
        assert_eq!(parse_position("1840"), None);
        assert_eq!(parse_position("x y"), None);
    }
}
