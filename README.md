# Claude Companion

A menu bar companion for Claude Code. A little ghost in your menu bar shows what Claude
is up to: white with no sessions, blue when connected, a bouncing purple while working,
orange with a badge when it needs your approval, and green when a task finishes. When a session needs permission, a popup appears in the corner with
**Allow**, **Deny** or **Go to terminal**, so you can answer without hunting for the tab.

Built with Tauri v2, so it can run on macOS, Windows and Linux. macOS is the first-class
target for now (see [Platform status](#platform-status)).

## How it works

```
Claude Code ──hook──▶ ~/.claude-companion/hook.sh ──POST──▶ 127.0.0.1:47823 (app)
            ◀─stdout── decision JSON (or nothing) ◀──────── popup answer
```

- The app registers a hook script for `SessionStart`, `UserPromptSubmit`, `PreToolUse`,
  `PostToolUse`, `PermissionRequest`, `Notification`, `Stop` and `SessionEnd`.
- Running sessions are discovered from `~/.claude/sessions/<pid>.json`, which Claude Code
  keeps updated with each session's status. This finds sessions started before the app, in any
  terminal or IDE. Hook events update status faster, and together they drive the tray ghost
  (white, blue, purple, orange, green).
- A `PermissionRequest` holds the hook open until you answer:
  - **Allow** / **Deny**: returns `hookSpecificOutput.decision` to Claude Code.
    Deny also interrupts Claude, like answering "No" in the terminal.
  - **Go to terminal**: returns no decision, so Claude Code shows its usual prompt,
    and the app brings that tab to the front.
  - If you don't answer within 9 minutes, it falls back to the terminal prompt.
- The popup never takes keyboard focus, ignores keyboard presses of its buttons, and ignores
  clicks for the first 0.6 seconds after it appears, so a click or Enter meant for another app
  can't approve anything.
- The popup opens in the top-right corner of the screen your mouse is on. Drag it anywhere
  (any empty spot works) and later popups open in the same place, even after a restart.
  **Reset popup position** in the tray menu puts it back.
- When the app isn't running, the hook's `curl` fails instantly and Claude Code
  behaves exactly as it does without the app.
- Some prompts don't come through the hook, for example ones raised before the hooks were installed.
  - **Terminal.app and iTerm:** **Allow** and **Deny** still work. The app reads the tab's
    screen, finds Claude's numbered menu, and types the option number into that exact tab,
    without switching windows. It types nothing unless the session is still waiting and the
    menu is on screen; otherwise it just opens the terminal.
  - **IDE terminals:** you get **Open <IDE>** instead.
- `AskUserQuestion` isn't held. You get a heads-up popup and answer in the terminal.
- No popup for the tab you're in: if a Terminal.app or iTerm session asks while its tab is in
  front, Claude Code asks right there. Switch into a session's tab while its popup is up and the
  popup hands the question to the terminal; leave with it unanswered and the popup comes back.
  IDE terminals don't say which tab is active, so there it's the tab you last typed into, if
  that was in the last 3 minutes and the IDE is still the app in front.
- When a task finishes, a green **Task finished** popup shows what you asked and Claude's
  last message, with **Open <terminal>** to jump to it. It closes itself after 12 seconds,
  and waits behind any permission popup.
- Sessions disappear from the tray menu within a couple of seconds of their Claude process
  exiting, even if the tab was killed without a `SessionEnd`. Each entry names the app it runs
  in, so a session still running in an IDE tab is easy to spot.

## Setup

```sh
# once
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
npm install

npm run dev          # run in development
npm run build -- --bundles app   # release build: src-tauri/target/release/bundle/macos/Claude Companion.app
```

Install it by copying the bundle to `/Applications` and opening it:

```sh
ditto "src-tauri/target/release/bundle/macos/Claude Companion.app" "/Applications/Claude Companion.app"
open "/Applications/Claude Companion.app"
```

- On first launch the installed app turns on **Start at Login** (a LaunchAgent at
  `~/Library/LaunchAgents/Claude Companion.plist`). You can toggle it in the tray menu.
- Only one copy runs at a time.
- The bundle is ad-hoc signed. That's fine for running it yourself; sharing it with others
  needs a Developer ID signature and notarization.

On first launch the app installs its hooks automatically. After that, the menu offers
**Remove Claude Code hooks** and **Install Claude Code hooks**.

- This adds entries to `~/.claude/settings.json`. Your existing hooks are left alone,
  and a backup is saved as `settings.json.companion-bak`.
- Running sessions pick up the hooks without a restart.
- To see what the app knows, run `curl 127.0.0.1:47823/sessions`.

The first time you use **Go to terminal** with iTerm2 or Terminal.app, macOS asks
whether Claude Companion may control that app. Allow it.

### Trying it without Claude

With the app running:

```sh
scripts/simulate.sh start
scripts/simulate.sh work         # icon blinks
scripts/simulate.sh permission   # popup; the decision JSON prints when you click
scripts/simulate.sh stop
scripts/simulate.sh end
```

## Troubleshooting

- **The icon stays white while Claude is running.** The hooks aren't installed, or the session started before they were. Pick *Install Claude Code hooks* from the tray menu, then start a new Claude session.
- **Allow/Deny doesn't type into the terminal.** macOS asks once for permission to control Terminal or iTerm. If you clicked *Don't Allow*, turn it back on in System Settings → Privacy & Security → Automation.
- **The popup opens off to the side.** It reopens wherever you last dragged it. Use *Reset popup position* in the tray menu to put it back in the top-right corner.
- **A session you closed is still in the menu.** Its Claude process is still running. Check the app named next to it; IDE terminals often keep running after their panel is hidden.
- **The badge is orange but no popup appears.** After the Mac wakes from sleep, macOS can stop putting the popup on screen. The app checks for this every couple of seconds: it shows the popup again, and if that doesn't work, it relaunches itself (at most once every 10 minutes). Each attempt is logged in `~/.claude-companion/popup.log`.
- **Seeing what the app knows.** `curl -s 127.0.0.1:47823/sessions` lists the sessions it's tracking.

## Platform status

| | macOS | Linux | Windows |
|---|---|---|---|
| Tray icon, status, popup | ✅ | should work (needs AppIndicator on GNOME) | should work |
| Hook script | ✅ | ✅ | needs Git Bash `curl` (or switch to `http` hooks) |
| Go to terminal | iTerm2 and Terminal.app focus the exact tab; VS Code and Cursor focus the project window; JetBrains IDEs and other terminals are brought to the front | not yet | not yet |

## Layout

- `src-tauri/src/server.rs`: HTTP endpoint and the permission round trip
- `src-tauri/src/state.rs`: sessions and pending requests
- `src-tauri/src/ui.rs`: tray icon animation, tray menu, showing and hiding the popup
- `src-tauri/src/discover.rs`: finds running sessions from Claude Code's session files
- `src-tauri/src/placement.rs`: where the popup opens, and remembering where you dragged it
- `src-tauri/src/panel.rs`: macOS only; makes the popup float over every Space and full-screen app
- `src-tauri/src/login.rs`: the Start at Login toggle
- `src-tauri/src/focus.rs`: finding the Claude process and its terminal tab
- `src-tauri/src/hooks_install.rs`: installs and removes hooks in `~/.claude/settings.json`
- `ui/`: the popup (plain HTML/CSS/JS, no bundler)
- `scripts/gen-icons.mjs`: draws the ghost icons as SVG and renders them to PNG (`npm run icons` also rebuilds the bundle icons)
