# Safety

shuttle acts on your own desktop, with your accounts signed in. These controls land with the desk tools in P1 (macOS), P2 (Windows) and P3 (Linux); v0.0.1 runs no desktop action.

## Kill switch

A global hotkey stops shuttle at once: it kills the running `gui_plan` and `script_run` processes, cancels the desk calls in flight and halts. Every tool call then fails with `stopped by the user` until you run `shuttle resume` or resume from the indicator.

| OS | Default | Registered with |
|---|---|---|
| macOS | ⌃⌥⌘⎋ (Control-Option-Command-Escape) | `RegisterEventHotKey`, which needs no permission |
| Windows | Ctrl+Alt+Shift+Esc | `RegisterHotKey` |
| Linux (X11) | Ctrl+Alt+Shift+Esc | `XGrabKey` |

Set `kill_hotkey` in [config.md](config.md) to change it. On Wayland a global hotkey depends on the compositor's GlobalShortcuts portal; where there is none, `shuttle stop` from a terminal is the kill switch.

## Indicator

While shuttle acts, a menu-bar item (macOS) or tray icon (Windows, Linux) shows it, and cua-driver draws its agent cursor where shuttle points. `indicator = false` turns both off.

## Confirm mode

`confirm = "coordinate"` asks before every click at a screen point, the least predictable action. `confirm = "every"` asks before every step that changes something. The question is a native dialog on your screen; a step you decline fails with `declined by the user`.

## App allowlist

`apps = ["TextEdit", "com.apple.Safari", "notepad.exe"]` limits the desk calls to windows of those apps, matched by app name, macOS bundle id or Windows executable. The daemon checks the window's process on every call, and the window under the point for a coordinate click. An empty list allows every app.

## What these controls do not cover

`script_run` runs JavaScript with your user's rights. The allowlist and the confirm mode bound the desk calls a script makes, not the script's own code, which can read files or start programs. If you do not want that, set `script_run = false`; `gui_plan`, `desk` and `screenshot` keep working.

## Input on macOS

macOS has one pointer and one keyboard focus, shared by you and shuttle. shuttle uses them as little as it can:

1. **Accessibility actions first.** A press, a toggle, a menu pick or a text value goes through the element's AX action. The cursor does not move and the focus does not change.
2. **Keys to the app.** Key presses go to the target app's process (`CGEventPostToPid`), so the app does not need to be in front. Some apps read keys only from the frontmost window; for those shuttle brings the window forward first.
3. **Coordinate clicks last.** When an element exposes no action (canvases, some Electron and game UIs), shuttle clicks its point. It first waits until you have not touched the mouse or keyboard for `user_idle_ms`, then moves, clicks and puts the cursor back. You still see the cursor jump, and a click can land on something you opened in that instant.

On Windows the same order holds with UI Automation patterns and `SendInput`. On Linux the X11 server has one core pointer as well; AT-SPI actions come first there too.
