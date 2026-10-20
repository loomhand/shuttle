# Safety

shuttle acts on your own desktop, with your accounts signed in. The controls below are built on macOS (P1) and Windows (P2). On Linux, with its desk backend, `cocoon-shuttle stop` and `resume`, the `script_run` switch and the app allowlist are built; its hotkey, indicator and confirm dialog are not yet.

## What these controls are

The kill switch, the app allowlist and the confirm mode are guard rails for an agent that means well and reaches for the wrong app or acts at the wrong time. They bound the `desk`, `screenshot` and `gui_plan` tools. They are not a sandbox: with `script_run` on, which is the default, a script is your own user's code, and it can resume a stopped daemon, edit `config.toml`, restart the daemon or start programs, and no check inside shuttle stops it. A [service plugin](#service-plugins) is your own user's code in the same way. The guard rails hold only with `script_run = false` under `[tools]` and no service plugin installed.

## Kill switch

`cocoon-shuttle stop` stops shuttle at once: it kills the running `gui_plan` or `script_run` process with every process it started (on macOS its process group, on Windows its Job object), ends the desk and model calls in flight, and halts. Every tool call except `status` then fails with `stopped by the user` until you resume, with `cocoon-shuttle resume` or the indicator's Resume. The stop lives in the running daemon; a daemon that starts again starts running. Only you stop and resume, from the command line, the hotkey or the indicator's menu: no MCP tool and no `gui_plan` step reaches them. A desk action that the driver or the accessibility API has already started ends on its own, so at most one action in flight completes after the stop. Likewise a window list or tree read in flight cannot be cancelled: a call after resume can queue behind it, at most until its own accessibility limit (3 s; 10 s for `snapshot`, `find` and `press`), and then fails with `no accessibility answer`.

A global hotkey does the same stop, and the indicator's menu has Stop and Resume. The hotkey is built on macOS and Windows; Linux does not have it yet, and its row is the planned default:

| OS | Default | Registered with |
|---|---|---|
| macOS | ⌃⌥⌘⎋ (Control-Option-Command-Escape) | `RegisterEventHotKey`, which needs no permission |
| Windows | Ctrl+Alt+Shift+Esc | `RegisterHotKey` |
| Linux (X11) | Ctrl+Alt+Shift+Esc | `XGrabKey` |

Set `kill_hotkey` in [config.md](config.md) to change it, or to `off` to register no global hotkey (the menu's Stop and `cocoon-shuttle stop` remain). On macOS a system permission prompt in front holds the hotkey until the prompt closes; `cocoon-shuttle stop` works regardless. On Wayland a global hotkey depends on the compositor's GlobalShortcuts portal; where there is none, `cocoon-shuttle stop` from a terminal is the kill switch.

## Indicator

On macOS a menu-bar item shows `Shuttle`, `● Shuttle` while a desk action or a run is in progress, and `■ Shuttle stopped` after a stop; its menu has Stop and Resume. `indicator = false` hides the menu-bar item. cua-driver draws its agent cursor where shuttle points either way, since macOS needs its overlay to track the front window; on Windows `[driver] overlay = false` in [config.md](config.md) turns the cursor off. On Windows a notification-area icon shows the same three states, as the stock application, information and error icons with the tooltip `Shuttle`, `Shuttle acting` or `Shuttle stopped`; a click opens its Stop and Resume menu. Windows 11 puts a new icon under the ^ overflow until you drag it onto the taskbar. `indicator = false` hides it. Linux has no tray icon yet.

## Confirm mode

`confirm = "coordinate"` asks before every step that can end in a click at a screen point, the least predictable action: `click`, `press`, `act` with `click`, and `drag`, which presses at one point and releases at another. `confirm = "every"` asks before every desk action: `press`, `act`, `key`, `type`, `click`, `scroll`, `drag`, `focus`, `close` and `launch`. `desk computer_act` is an action too: `coordinate` asks before its `tap`, `click`, `left_click`, `double_tap`, `double_click`, `right_click`, `triple_click`, `long_press`, `swipe` and `drag`, its `type` at an element or a point, which clicks there first, and a list that holds any of them; `every` asks before each act. On macOS the question is a dialog with Cancel and Allow: Escape presses Cancel and Return presses neither. A step you cancel fails with `declined by the user`, and a stop closes an open dialog. While the dialog is open every other desk call fails at once with `a confirm dialog is open`, so the agent cannot answer the dialog itself. On Windows the question is a message box with OK and Cancel; Cancel is the default, so Return and Escape both refuse. The box opens in front of other windows with the keyboard focus; if Windows refuses it the focus, its taskbar button flashes instead. Linux has no dialog yet, so a step that would ask fails and names `confirm = "off"`. With `confirm = "every"`, a `gui_plan` `app` step for an app that is not running asks twice, first to focus it and then to launch it; declining the focus does not end the step, and the launch still asks.

## App allowlist

`apps = ["TextEdit", "notepad.exe"]` under `[safety]` limits the desk calls to windows of those apps. A name matches the macOS app name, the Windows executable or the Linux executable's file name (`/proc/PID/exe`), case-insensitive and with or without `.exe`; a Linux window whose executable cannot be read is refused. The daemon checks the app on every desk call:
- a window name picks only among the allowed apps' windows, and an error lists only those; with no name, the focused window must belong to an allowed app;
- the focused window for `key` and `type`;
- the process of the element for `read`, `press` and `act`, and of the element `at` finds;
- the window under the point for every click at a point, from `click`, `press` or `act`, and the topmost element there across every window layer, such as the Dock or the taskbar; on macOS, an open menu item at the point instead.

`windows` lists only the allowed apps' windows, and `launch` and a `gui_plan` app step start only an allowed app. The computer calls of [wire.md](wire.md#computer-calls), which see and click the whole screen, are refused while the list is not empty. A screenshot or crop, also the one a model call gets, is black wherever no allowed app's window is on top: the desktop, the menu bar, the Dock or taskbar, other apps' windows, and on macOS also open menus. An empty list allows every app and costs nothing. A check reads the window list once, which costs a few ms to a few hundred ms on macOS depending on the apps on screen.

The check runs before the action, not at the input event: a window that comes to the front in between can take a key or a click. A screenshot and a click each read the window list on their own, so they can see different windows.

The screenshot mask is per window rectangle, not per pixel:
- a translucent allowed window shows what is composited behind it;
- a menu, tooltip or popover of another app, the Dock or the taskbar over an allowed window stays visible inside that window's rectangle;
- a window that is on screen at the capture but gone when the window list is read stays visible. A process with no window of its own, such as a helper or a renderer, is refused. Without a `config.toml` there is no allowlist.

## A shared endpoint

`[daemon] socket_group` ([config.md](config.md#daemon-endpoint)) lets every member of that group call the daemon as you do:
- every desk verb, on your desktop and with your signed-in apps;
- `gui_plan` and, with `script_run` on, `script_run` and the plugin tools, which run as your user;
- the model calls on your key, which stays in the daemon but pays for theirs.

The daemon reads the user of each connection's peer and answers `safety_stop` and `safety_resume` only from its own user, so a member cannot send them directly. That keeps the stop yours only while `script_run` is off and no service plugin is installed: a member's script or service runs as your user, and its calls, the stop and the resume among them, pass as yours. The app allowlist and the confirm mode apply to their calls as to yours. Give the group only to accounts you would let use your desktop; `script_run = false` keeps them to the desk tools.

## Turning script_run off

`script_run = false` under `[tools]` turns off the one tool that runs the agent's own code, and the plugin tools of [wire.md](wire.md#plugins) with it: the MCP server lists none, and `script_run` and every plugin answer how to turn it back on. `gui_plan`, `desk` and `screenshot` keep working, and so do [service plugins](#service-plugins). While plugins run, their desk calls pass the same app allowlist, confirm and stop as any other.

## Service plugins

A [service plugin](wire.md#service-plugins) is a program you install in the plugin directory; the daemon runs it as your user and hands it the calls of its tools. Installing it is what turns it on: `script_run = false` does not turn it off, so that the tools it serves keep working where scripts are off.
- **Stop.** `cocoon-shuttle stop` refuses new calls of its tools and closes every one in flight. The service itself keeps running, and work it started for a call, such as a shell command, is its own to end when the connection closes; shuttle does not end it.
- **App allowlist and confirm.** They do not cover what a service does itself, such as reading files or running commands. Only the desk calls it makes on `SHUTTLE_SOCKET` pass them, as any other desk call.
- **Reach.** Its socket is in a directory only your user reaches (0700), so a call reaches it only through the daemon, with the stop applied.

## Input on macOS

macOS has one pointer and one keyboard focus, shared by you and shuttle:

1. **Accessibility actions.** A press runs a menu entry's AX action, clicks any other element's centre when a hit test there finds it, and runs its AX action otherwise. A click at a point on an open menu item runs that item's AX action.
2. **Keys to the desktop.** Keys and typed text go through cua-driver to the focused window, so the target window must be in front.
3. **Coordinate clicks.** A click at a point goes through cua-driver to the window under it, which shuttle brings to the front first; a window that refuses pointer input, such as a sheet, gets a desktop click. shuttle does not wait for you to stop using the mouse or keyboard, so a click can land on something you opened in that instant.

On Windows the same order holds with UI Automation patterns and cua-driver input. On Linux AT-SPI actions come first too.

## Background input

With `[desk] input = "background"` ([config.md](config.md#desk-input)), shuttle sends input to its target window through cua-driver without fronting that window or moving your pointer. It never falls back to the foreground; an action that needs the foreground fails and names why:
- keys, typed text, scrolls and drags need the target window named (`window` or `pid`);
- a window that takes no pointer input without the foreground, such as a macOS sheet, refuses a click;
- on Linux the computer actions refuse, since they send desktop input;
- `gui_plan` refuses, since it drives the front window.

The driver refuses or cannot deliver some input in the background:
- macOS drag and drop follows the real pointer;
- modifier clicks need the foreground;
- some toolkits (Tk, LibreOffice) refuse background input;
- on X11, GTK and Chromium ignore synthetic events, and Chrome drops core keyboard input while a second master keyboard exists. Second-cursor delivery needs a real Xorg server and `/dev/uinput`.

A delivered action is not always a landed one. When cua-driver cannot confirm that background input took effect, for example keys posted to a Windows XAML or console window, the result carries `unverified` with the driver's note ([wire.md](wire.md#desk-calls)). Read the effect back before relying on it; the input may have landed, so sending it again can type twice. An app can also bring itself to the front when it opens a menu or dialog; shuttle sends no fronting call, but cannot stop the app.

`focus` is the one call that brings a window to the front in background mode.
