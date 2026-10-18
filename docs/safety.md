# Safety

shuttle acts on your own desktop, with your accounts signed in. The controls below are built on macOS (P1) and Windows (P2); Linux gets them in P3, together with its desk backend.

## What these controls are

The kill switch, the app allowlist and the confirm mode are guard rails for an agent that means well and reaches for the wrong app or acts at the wrong time. They bound the `desk`, `screenshot` and `gui_plan` tools. They are not a sandbox: with `script_run` on, which is the default, a script is your own user's code, and it can resume a stopped daemon, edit `config.toml`, restart the daemon or start programs, and no check inside shuttle stops it. The guard rails hold only with `script_run = false` under `[tools]`.

## Kill switch

`cocoon-shuttle stop` stops shuttle at once: it kills the running `gui_plan` or `script_run` process (on macOS with its process group; on Windows only the Node process, so programs a script started keep running), ends the desk and model calls in flight, and halts. Every tool call except `status` then fails with `stopped by the user` until you resume, with `cocoon-shuttle resume` or the indicator's Resume. The stop lives in the running daemon; a daemon that starts again starts running. Only you stop and resume, from the command line, the hotkey or the indicator's menu: no MCP tool and no `gui_plan` step reaches them. A desk action that the driver or the accessibility API has already started ends on its own, so at most one action in flight completes after the stop. Likewise a window list or tree read in flight cannot be cancelled: a call after resume can queue behind it, at most until its own accessibility limit (3 s; 10 s for `snapshot`, `find` and `press`), and then fails with `no accessibility answer`.

A global hotkey does the same stop, and the indicator's menu has Stop and Resume. The hotkey is built on macOS and Windows; Linux gets it in P3:

| OS | Default | Registered with |
|---|---|---|
| macOS | ⌃⌥⌘⎋ (Control-Option-Command-Escape) | `RegisterEventHotKey`, which needs no permission |
| Windows | Ctrl+Alt+Shift+Esc | `RegisterHotKey` |
| Linux (X11) | Ctrl+Alt+Shift+Esc | `XGrabKey` |

Set `kill_hotkey` in [config.md](config.md) to change it. On macOS a system permission prompt in front holds the hotkey until the prompt closes; `cocoon-shuttle stop` works regardless. On Wayland a global hotkey depends on the compositor's GlobalShortcuts portal; where there is none, `cocoon-shuttle stop` from a terminal is the kill switch.

## Indicator

On macOS a menu-bar item shows `Shuttle`, `● Shuttle` while a desk action or a run is in progress, and `■ Shuttle stopped` after a stop; its menu has Stop and Resume. `indicator = false` hides the menu-bar item. cua-driver draws its agent cursor where shuttle points either way, since macOS needs its overlay to track the front window. On Windows a notification-area icon shows the same three states, as the stock application, information and error icons with the tooltip `Shuttle`, `Shuttle acting` or `Shuttle stopped`; a click opens its Stop and Resume menu. Windows 11 puts a new icon under the ^ overflow until you drag it onto the taskbar. `indicator = false` hides it. The Linux tray icon comes in P3.

## Confirm mode

`confirm = "coordinate"` asks before every step that can end in a click at a screen point, the least predictable action: `click`, `press`, and `act` with `click`. `confirm = "every"` asks before every desk action: `press`, `act`, `key`, `type`, `click`, `focus`, `close` and `launch`. On macOS the question is a dialog with Cancel and Allow: Escape presses Cancel and Return presses neither. A step you cancel fails with `declined by the user`, and a stop closes an open dialog. While the dialog is open every other desk call fails at once with `a confirm dialog is open`, so the agent cannot answer the dialog itself. On Windows the question is a message box with OK and Cancel; Cancel is the default, so Return and Escape both refuse. The box opens in front of other windows with the keyboard focus; if Windows refuses it the focus, its taskbar button flashes instead. Linux gets the dialog in P3; until then a step that would ask fails and names `confirm = "off"`.

## App allowlist

`apps = ["TextEdit", "notepad.exe"]` under `[safety]` limits the desk calls to windows of those apps. A name matches the macOS app name or the Windows executable, case-insensitive and with or without `.exe`. The daemon checks the app on every desk call:
- a window name picks only among the allowed apps' windows, and an error lists only those; with no name, the focused window must belong to an allowed app;
- the focused window for `key` and `type`;
- the process of the element for `read`, `press` and `act`, and of the element `at` finds;
- the window under the point for every click at a point, from `click`, `press` or `act`, and the topmost element there across every window layer, such as the Dock or the taskbar; on macOS, an open menu item at the point instead.

`windows` lists only the allowed apps' windows, and `launch` and a `gui_plan` app step start only an allowed app. A screenshot or crop, also the one a model call gets, is black wherever no allowed app's window is on top: the desktop, the menu bar, the Dock or taskbar, other apps' windows, and on macOS also open menus. An empty list allows every app and costs nothing. A check reads the window list once, which costs a few ms to a few hundred ms on macOS depending on the apps on screen.

The check runs before the action, not at the input event: a window that comes to the front in between can take a key or a click. A screenshot and a click each read the window list on their own, so they can see different windows.

The screenshot mask is per window rectangle, not per pixel:
- a translucent allowed window shows what is composited behind it;
- a menu, tooltip or popover of another app, the Dock or the taskbar over an allowed window stays visible inside that window's rectangle;
- a window that is on screen at the capture but gone when the window list is read stays visible. A process with no window of its own, such as a helper or a renderer, is refused. Without a `config.toml` there is no allowlist.

## Turning script_run off

`script_run = false` under `[tools]` turns off the one tool that runs the agent's own code. `script_run` then answers how to turn it back on, and `gui_plan`, `desk` and `screenshot` keep working.

## Input on macOS

macOS has one pointer and one keyboard focus, shared by you and shuttle:

1. **Accessibility actions.** A press runs a menu entry's AX action, clicks any other element's centre when a hit test there finds it, and runs its AX action otherwise. A click at a point on an open menu item runs that item's AX action.
2. **Keys to the desktop.** Keys and typed text go through cua-driver to the focused window, so the target window must be in front.
3. **Coordinate clicks.** A click at a point goes through cua-driver to the window under it, which shuttle brings to the front first; a window that refuses pointer input, such as a sheet, gets a desktop click. shuttle does not wait for you to stop using the mouse or keyboard, so a click can land on something you opened in that instant.

On Windows the same order holds with UI Automation patterns and cua-driver input. On Linux AT-SPI actions come first too.
