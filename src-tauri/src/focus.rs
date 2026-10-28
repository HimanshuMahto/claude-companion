//! Bringing a Claude Code session's terminal tab to the front.

use std::process::Command;

use crate::state::Terminal;

/// How recently you must have typed into an IDE terminal tab to count as being in it.
#[cfg(target_os = "macos")]
const TYPED_WITHIN: std::time::Duration = std::time::Duration::from_secs(3 * 60);

const SHELLS: &[&str] = &["sh", "bash", "zsh", "fish", "dash", "nu", "login"];

/// Walks up from the hook script's parent process to the Claude Code process
/// (the first ancestor that isn't a shell) and returns its pid and tty.
#[cfg(unix)]
pub fn resolve_claude_process(mut pid: u32) -> Option<(u32, Option<String>)> {
    for _ in 0..8 {
        let out = Command::new("ps")
            .args(["-o", "ppid=,tty=,comm=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let mut parts = line.split_whitespace();
        let ppid: u32 = parts.next()?.parse().ok()?;
        let tty = parts.next().unwrap_or("?");
        let comm = parts.collect::<Vec<_>>().join(" ");
        let name = comm.rsplit('/').next().unwrap_or("").trim_start_matches('-');
        if !SHELLS.contains(&name) {
            let tty = (!tty.starts_with('?')).then(|| format!("/dev/{tty}"));
            return Some((pid, tty));
        }
        if ppid <= 1 {
            return None;
        }
        pid = ppid;
    }
    None
}

#[cfg(not(unix))]
pub fn resolve_claude_process(pid: u32) -> Option<(u32, Option<String>)> {
    Some((pid, None))
}

#[cfg(unix)]
pub fn is_alive(pid: u32) -> bool {
    // Signal 0 checks existence without delivering anything. EPERM still means it exists.
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    ret == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Reads a running Claude process's tty and terminal-identifying environment.
#[cfg(unix)]
pub fn process_terminal(pid: u32) -> Terminal {
    let ps = |args: &[&str]| {
        Command::new("ps")
            .args(args)
            .args(["-p", &pid.to_string()])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let tty = ps(&["-o", "tty="]);
    // `ps eww` appends the environment to the command line (own processes only).
    let env = ps(&["eww", "-o", "command="]);
    let var = |key: &str| {
        env.split_whitespace()
            .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
            .unwrap_or_default()
            .to_string()
    };
    Terminal {
        claude_pid: Some(pid),
        tty: (!tty.is_empty() && !tty.starts_with('?')).then(|| format!("/dev/{tty}")),
        term_program: var("TERM_PROGRAM"),
        bundle_id: var("__CFBundleIdentifier"),
        iterm_session: var("ITERM_SESSION_ID"),
    }
}

#[cfg(not(unix))]
pub fn process_terminal(pid: u32) -> Terminal {
    Terminal { claude_pid: Some(pid), ..Default::default() }
}

#[cfg(not(unix))]
pub fn is_alive(_pid: u32) -> bool {
    true
}

/// Human name of the app hosting a session, for the popup's button.
pub fn app_name(t: &Terminal) -> String {
    let known = match t.bundle_id.as_str() {
        "com.apple.Terminal" => "Terminal",
        "com.googlecode.iterm2" => "iTerm",
        "com.microsoft.VSCode" => "VS Code",
        "com.todesktop.230313mzl4w4u92" => "Cursor",
        "com.jetbrains.intellij" | "com.jetbrains.intellij.ce" => "IntelliJ IDEA",
        "com.jetbrains.WebStorm" => "WebStorm",
        "com.jetbrains.pycharm" | "com.jetbrains.pycharm.ce" => "PyCharm",
        "com.jetbrains.goland" => "GoLand",
        "com.mitchellh.ghostty" => "Ghostty",
        "dev.warp.Warp-Stable" => "Warp",
        "com.github.wez.wezterm" => "WezTerm",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_string();
    }
    match t.term_program.as_str() {
        "iTerm.app" => "iTerm".into(),
        "Apple_Terminal" => "Terminal".into(),
        "vscode" => "VS Code".into(),
        "" => t.bundle_id.rsplit('.').next().unwrap_or("").to_string(),
        other => other.to_string(),
    }
}

#[cfg(target_os = "macos")]
pub fn focus_terminal(t: &Terminal, cwd: &str) {
    let ok = match t.term_program.as_str() {
        "iTerm.app" => focus_iterm(t),
        "Apple_Terminal" => focus_apple_terminal(t),
        // VS Code, Cursor, Windsurf...: reopening the folder focuses its window.
        "vscode" if !cwd.is_empty() => open_app(t, &[cwd]),
        // JetBrains IDEs don't set TERM_PROGRAM; same trick works for them.
        _ if t.bundle_id.starts_with("com.jetbrains.") && !cwd.is_empty() => open_app(t, &[cwd]),
        _ => false,
    };
    if !ok {
        open_app(t, &[]);
    }
}

#[cfg(target_os = "macos")]
fn focus_iterm(t: &Terminal) -> bool {
    in_iterm_session(t, "tell w to select\ntell t to select\ntell s to select\nactivate\nreturn \"ok\"")
        .is_some_and(|out| out == "ok")
}

#[cfg(target_os = "macos")]
fn focus_apple_terminal(t: &Terminal) -> bool {
    in_terminal_tab(t, "set selected of t to true\nset index of w to 1\nactivate\nreturn \"ok\"")
        .is_some_and(|out| out == "ok")
}

/// Runs `body` with `w`/`t`/`s` bound to the session's iTerm window/tab/session.
#[cfg(target_os = "macos")]
fn in_iterm_session(t: &Terminal, body: &str) -> Option<String> {
    // ITERM_SESSION_ID looks like "w0t1p0:6A2C...": the part after ':' is the unique id.
    let uid = t.iterm_session.split(':').nth(1).unwrap_or("");
    let tty = t.tty.as_deref().unwrap_or("");
    if uid.is_empty() && tty.is_empty() {
        return None;
    }
    osascript(&format!(
        r#"tell application "iTerm"
  repeat with w in windows
    repeat with t in tabs of w
      repeat with s in sessions of t
        if (unique id of s is "{uid}") or (tty of s is "{tty}") then
          {body}
        end if
      end repeat
    end repeat
  end repeat
end tell"#,
        uid = escape(uid),
        tty = escape(tty),
    ))
}

/// Runs `body` with `w`/`t` bound to the session's Terminal.app window/tab.
#[cfg(target_os = "macos")]
fn in_terminal_tab(t: &Terminal, body: &str) -> Option<String> {
    let tty = t.tty.as_deref()?;
    // Finished tabs keep reporting the tty they had, and macOS reuses tty names,
    // so only a busy (still running) tab can be ours.
    osascript(&format!(
        r#"tell application "Terminal"
  repeat with w in windows
    repeat with t in tabs of w
      if tty of t is "{tty}" and busy of t then
        {body}
      end if
    end repeat
  end repeat
end tell"#,
        tty = escape(tty),
    ))
}

/// Whether the user is looking at this session's tab right now. Terminal.app
/// and iTerm say which tab is selected in their front window. IDE terminals
/// don't, so for those it's the tab you last typed into, if that was recent
/// and the IDE is still the frontmost app.
#[cfg(target_os = "macos")]
pub fn is_in_front(t: &Terminal) -> bool {
    let (bundle, script) = match t.term_program.as_str() {
        "Apple_Terminal" => ("com.apple.Terminal", r#"tell application "Terminal" to return tty of selected tab of front window"#),
        "iTerm.app" => (
            "com.googlecode.iterm2",
            r#"tell application "iTerm" to tell current session of current window to return (unique id) & " " & tty"#,
        ),
        _ => {
            let Some(tty) = t.tty.as_deref() else { return false };
            return !t.bundle_id.is_empty()
                && frontmost_bundle().as_deref() == Some(t.bundle_id.as_str())
                && typed_here_last(tty);
        }
    };
    // Checked first so a scripting call never launches or wakes a terminal app.
    if frontmost_bundle().as_deref() != Some(bundle) {
        return false;
    }
    let Some(front) = osascript(script) else { return false };
    let uid = t.iterm_session.split(':').nth(1).unwrap_or("");
    let tty = t.tty.as_deref().unwrap_or("");
    front.split_whitespace().any(|id| (!uid.is_empty() && id == uid) || (!tty.is_empty() && id == tty))
}

/// Whether `tty` is the terminal the user typed into most recently, within
/// `TYPED_WITHIN`. A tty's access time moves when the program in it reads
/// keyboard input, and not when it prints.
#[cfg(target_os = "macos")]
fn typed_here_last(tty: &str) -> bool {
    let typed_at = |path: &std::path::Path| std::fs::metadata(path).and_then(|m| m.accessed()).ok();
    let Some(mine) = typed_at(std::path::Path::new(tty)) else { return false };
    if mine.elapsed().map_or(true, |e| e > TYPED_WITHIN) {
        return false;
    }
    let Ok(devices) = std::fs::read_dir("/dev") else { return false };
    devices
        .flatten()
        .filter(|d| d.file_name().to_string_lossy().starts_with("ttys"))
        .all(|d| typed_at(&d.path()).is_none_or(|other| other <= mine))
}

#[cfg(not(target_os = "macos"))]
pub fn is_in_front(_t: &Terminal) -> bool {
    false
}

/// Bundle id of the frontmost app, e.g. "com.apple.Terminal".
#[cfg(target_os = "macos")]
fn frontmost_bundle() -> Option<String> {
    let run = |args: &[&str]| Command::new("lsappinfo").args(args).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let front = run(&["front"])?;
    // Prints: "CFBundleIdentifier"="com.apple.Terminal"
    let info = run(&["info", "-only", "bundleid", &front])?;
    Some(info.rsplit_once('=')?.1.trim_matches('"').to_string())
}

#[derive(Clone, Copy, Debug)]
pub enum Answer {
    Allow,
    Deny,
}

/// Whether `answer_prompt` can type into this session's terminal.
pub fn can_type_answers(t: &Terminal) -> bool {
    cfg!(target_os = "macos")
        && match t.term_program.as_str() {
            "Apple_Terminal" => t.tty.is_some(),
            "iTerm.app" => t.tty.is_some() || !t.iterm_session.is_empty(),
            _ => false,
        }
}

/// Answers a permission prompt that Claude Code is showing in the terminal by
/// typing the matching option number, the way a user would. Reads the screen
/// first and types nothing unless Claude's Yes/No menu is actually there.
#[cfg(target_os = "macos")]
pub fn answer_prompt(t: &Terminal, answer: Answer) -> Result<(), String> {
    let iterm = t.term_program == "iTerm.app";
    let screen = if iterm {
        in_iterm_session(t, "tell s to return contents")
    } else {
        in_terminal_tab(t, "return history of t")
    }
    .ok_or("couldn't find the session's terminal tab")?;
    let key = prompt_option(&screen, answer).ok_or("Claude's permission menu isn't on screen")?;

    // Claude Code takes the option number; the Return after it is a no-op on
    // the empty input box once the menu has closed.
    let sent = if iterm {
        in_iterm_session(
            t,
            &format!("tell s\nwrite text \"{key}\" newline no\nwrite text (character id 13) newline no\nend tell\nreturn \"ok\""),
        )
    } else {
        // `do script` types the text followed by Return.
        in_terminal_tab(t, &format!("do script \"{key}\" in t\nreturn \"ok\""))
    };
    match sent.as_deref() {
        Some("ok") => Ok(()),
        _ => Err("couldn't type into the terminal".into()),
    }
}

#[cfg(not(target_os = "macos"))]
pub fn answer_prompt(_t: &Terminal, _answer: Answer) -> Result<(), String> {
    Err("not supported on this platform".into())
}

/// Picks the option number from the last Claude permission menu on screen:
///   ❯ 1. Yes
///     2. Yes, and always allow ...
///     3. No
fn prompt_option(screen: &str, answer: Answer) -> Option<u32> {
    let lines: Vec<&str> = screen.lines().collect();
    let tail = &lines[lines.len().saturating_sub(60)..];
    let start = tail
        .iter()
        .rposition(|l| parse_option(l).is_some_and(|(n, label)| n == 1 && label.starts_with("Yes")))?;
    match answer {
        Answer::Allow => Some(1),
        Answer::Deny => tail[start..]
            .iter()
            .filter_map(|l| parse_option(l))
            .find(|(_, label)| label.starts_with("No"))
            .map(|(n, _)| n),
    }
}

fn parse_option(line: &str) -> Option<(u32, &str)> {
    let s = line.trim_start().trim_start_matches('❯').trim_start();
    let (num, label) = s.split_once(". ")?;
    Some((num.parse().ok()?, label.trim()))
}

#[cfg(target_os = "macos")]
fn open_app(t: &Terminal, args: &[&str]) -> bool {
    if t.bundle_id.is_empty() {
        return false;
    }
    Command::new("open")
        .arg("-b")
        .arg(&t.bundle_id)
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn osascript(script: &str) -> Option<String> {
    let out = Command::new("osascript").args(["-e", script]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

#[cfg(target_os = "macos")]
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

// TODO: Windows (Windows Terminal has no per-tab focus API; raise the window)
// and Linux (wmctrl/xdotool on X11, compositor-specific on Wayland).
#[cfg(not(target_os = "macos"))]
pub fn focus_terminal(_t: &Terminal, _cwd: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    // Captured from a real Claude Code 2.1 Bash permission prompt.
    const BASH_PROMPT: &str = "\
❯ Use the Bash tool to run exactly: touch denied.txt
⏺ Bash(touch denied.txt)
 Bash command
 touch denied.txt
 Do you want to proceed?
 ❯ 1. Yes
   2. Yes, and always allow access to
      /tmp/kt from this
      project
   3. Yes, and switch to auto mode · auto mode handles these prompts for you
   4. No
 Esc to cancel · Tab to amend";

    #[test]
    fn picks_yes_and_the_no_option_number() {
        assert_eq!(prompt_option(BASH_PROMPT, Answer::Allow), Some(1));
        assert_eq!(prompt_option(BASH_PROMPT, Answer::Deny), Some(4));
    }

    #[test]
    fn uses_the_last_menu_on_screen() {
        let screen = format!("{BASH_PROMPT}\n⏺ done\n Do you want to make this edit?\n ❯ 1. Yes\n   2. Yes, allow all edits\n   3. No");
        assert_eq!(prompt_option(&screen, Answer::Deny), Some(3));
    }

    #[test]
    fn types_nothing_without_a_menu() {
        let screen = "⏺ I ran touch allowed.txt.\n❯ \n  ⏸ manual mode on";
        assert_eq!(prompt_option(screen, Answer::Allow), None);
        assert_eq!(prompt_option(screen, Answer::Deny), None);
    }
}
