# Safety

shuttle acts on your own desktop, with your accounts signed in. These controls land in P1 (macOS), P2 (Windows) and P3 (Linux). The released v0.0.1 runs no desktop action; `main` (unreleased) already acts on the desktop as a developer preview, without any of them.

## Kill switch

`cocoon-shuttle stop` stops shuttle at once: it kills the running `gui_plan` or `script_run` process with its process group, ends the desk and model calls in flight, and halts. Every tool call then fails with `stopped by the user` until you run `cocoon-shuttle resume`. The stop lives in the running daemon; a daemon that starts again starts running.

A global hotkey for the same stop, and Resume from the indicator, come with the macOS controls below (not built yet):

| OS | Default | Registered with |
|---|---|---|
| macOS | ⌃⌥⌘⎋ (Control-Option-Command-Escape) | `RegisterEventHotKey`, which needs no permission |
| Windows | Ctrl+Alt+Shift+Esc | `RegisterHotKey` |
| Linux (X11) | Ctrl+Alt+Shift+Esc | `XGrabKey` |

Set `kill_hotkey` in [config.md](config.md) to change it. On Wayland a global hotkey depends on the compositor's GlobalShortcuts portal; where there is none, `cocoon-shuttle stop` from a terminal is the kill switch.

## Indicator (not built yet)

While shuttle acts, a menu-bar item (macOS) or tray icon (Windows, Linux) shows it, and cua-driver draws its agent cursor where shuttle points. `indicator = false` turns both off.

## Confirm mode (not built yet)

`confirm = "coordinate"` asks before every click at a screen point, the least predictable action. `confirm = "every"` asks before every step that changes something. The question is a native dialog on your screen; a step you decline fails with `declined by the user`.

## App allowlist

`apps = ["TextEdit", "notepad.exe"]` under `[safety]` limits the desk calls to windows of those apps. A name matches the macOS app name or the Windows executable, case-insensitive and with or without `.exe`. The daemon checks the app on every desk call:
- the window a call names, or the focused window when it names none;
- the focused window for `key` and `type`;
- the process of the element for `read`, `press` and `act`;
- the window under the point for `click`.

`windows` lists only the allowed apps' windows, and `launch` starts only an allowed app. `shot` and `screen` are not bound to a window and still show the whole screen. An empty list allows every app. Each check reads the window list once, which costs a few ms to a few hundred ms on macOS depending on the apps on screen.

## What these controls do not cover

`script_run` runs JavaScript with your user's rights. The allowlist and the confirm mode bound the desk calls a script makes, not the script's own code, which can read files or start programs. If you do not want that, set `script_run = false` under `[tools]`; `script_run` then answers how to turn it back on, and `gui_plan`, `desk` and `screenshot` keep working.

## Input on macOS

This is the P1 design. Today a press runs a menu entry's AX action, clicks any other element's centre when a hit test there finds it, and runs its AX action otherwise; keys go to the desktop through cua-driver.

macOS has one pointer and one keyboard focus, shared by you and shuttle. shuttle uses them as little as it can:

1. **Accessibility actions first.** A press, a toggle, a menu pick or a text value goes through the element's AX action. The cursor does not move and the focus does not change.
2. **Keys to the app.** Key presses go to the target app's process (`CGEventPostToPid`), so the app does not need to be in front. Some apps read keys only from the frontmost window; for those shuttle brings the window forward first.
3. **Coordinate clicks last.** When an element exposes no action (canvases, some Electron and game UIs), shuttle clicks its point. It first waits until you have not touched the mouse or keyboard for `user_idle_ms`, then moves, clicks and puts the cursor back. You still see the cursor jump, and a click can land on something you opened in that instant.

On Windows the same order holds with UI Automation patterns and `SendInput`. On Linux the X11 server has one core pointer as well; AT-SPI actions come first there too.
