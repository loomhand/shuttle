# Changelog

## Unreleased

- **Android taps and keys from the UI server.** A tap and a single key press are injected by the UI server through `InputManager`, as `adb shell input` does it, in place of an adb process and a device JVM per call:
  - the UI line protocol gains `tap X Y` and `key CODE`, each answered `true` (both events accepted), `false` (one was not, and the press may still have happened) or `unavailable` (nothing could be injected);
  - `unavailable`, a server that predates the requests and a server that does not start leave the call to `adb shell input`, the first two for good; a `false`, an error or a lost reply is an error that says the press may have reached the device, and is never repeated by adb;
  - a tap or key takes 0.6–1.4 ms against 16.5 ms and about 85–90% less CPU on redroid 16;
  - key combinations, swipes and long presses stay on `adb shell input`: a swipe the server injects flings about 1% farther than `input swipe`, enough to move a list off a saved position over repeated scrolls.
- **MCP cancellation.** `cocoon-shuttle mcp` keeps reading its client while a request runs, and a `notifications/cancelled` drops the request it names, running or queued, without a reply; a running one closes its daemon connection, where the daemon ends the call. Before, the server read the next line only after the call had ended, so an interrupt reached it up to the 110 s run limit late while the desktop kept acting.
- **Android screenshots from the UI server.** `desk_look`, `desk_shot`, `computer_state` and every `computer_act` take the screen from the UI server's `UiAutomation.takeScreenshot` in place of an `adb exec-out screencap -p` process:
  - the UI line protocol gains a `shot` request answered by a binary frame, `png N` and then N bytes before the end line; a server that predates it answers a dump, and `screencap` serves for good after that, as it does for one call after a failed shot;
  - a capture takes 30 ms against 92–104 ms and about 55–60% less CPU on redroid 16;
  - an uncropped `desk_shot` returns that PNG as it is, with no decode and re-encode; a cropped one is cut as before.
- **Android press scrolls the element into view.** `press` on an element in a scrolling list that is off the screen first asks the element to scroll itself into view (`ACTION_SHOW_ON_SCREEN`, through a `show ID PATH` request of the UI server) in place of a 600 ms swipe and a 400 ms settle:
  - a scrolled press takes 138 ms against 1073 ms for one scroll and 137 ms against 4278 ms for three or four, on the Settings list of redroid 16;
  - the tree is read again after the request; an element that did not move, an element that refused, an error, or a server that predates the request leaves the press to the swipe and settle as before, and a press asks once;
  - lists that do not honour the action, Compose among them, were not tried; they fall back to the swipe.
- **Graceful run ends.** On macOS and Linux a run's timeout, a stop or a dropped client sends SIGTERM to the run's process group, so a script can release the keys it holds or close what it opened, and SIGKILL to whatever is left 2 s later; on Linux the apps the desk moved into scopes of their own are spared, as the bundled driver's end already spared them. A run still in that grace when its client goes away, or when the daemon shuts down, is killed all the same. Before, the group got SIGKILL at once. Windows keeps ending the run's Job object at once.

## 0.1.8 - 2026-10-20

- **Windows pipe security.** The daemon's named pipe admits SYSTEM and its own user only, with an explicit security descriptor in place of the default one, and `[daemon] socket_group` now works on Windows: an account name or SID string that may read and write the pipe but not create instances of it. `safety_stop` and `safety_resume` answer only a client whose token is the daemon's user, as on macOS and Linux; on Windows they answered any client before.
- **Windows service plugins.** Service plugins run on Windows: a service serves the named pipe the daemon names in `SHUTTLE_SERVICE_SOCKET`, the daemon's pipe name followed by `-svc-<service>`, and a call reaches it only when the process serving that pipe is in the service's Job object, so another process that took the pipe name first is refused with its pid named. A pipe has no half-close, so on Windows a close from either side ends a service call.
- **Windows process trees.** Each `script_run` or `gui_plan` run and each service plugin runs in a Job object of its own. A timeout, a stop or a dropped connection kills the run with every process it started, as the process group does on macOS and Linux, and a service dies with its whole tree when the daemon stops or exits by any means, `taskkill /F` and logoff included. A run that finishes leaves the processes it started running.
- **Tests without ETXTBSY.** The adb, cua-driver and plugin tests write their fake programs from a child shell, so a parallel `cargo test` no longer fails now and then with `Text file busy` when another test thread forks while the script is open.
- **Bundled:** cua-driver v0.37.0-loomhand.1 (was v0.36.0-loomhand.6): the fork's patches on upstream cua-driver-rs 0.37.0, which brings background drags in Electron windows, macOS window screenshots without the pointer, a macOS focus check before keys, a locked-screen report and X11 windows without a title.

## 0.1.7 - 2026-10-20

- **Android UI server name.** The UI server is the class `shuttle.Ui` in `shuttle-ui.jar`: the linux archives carry it as `android/shuttle-ui.jar`, which an `[android]` section without `ui_jar` takes, and the device copies are `/data/local/tmp/shuttle-ui.jar` and `/data/local/tmp/shuttle-scrcpy.jar`. A configured `ui_jar` keeps working whatever its file name.
- **Smaller UI server answers.** The dump no longer carries `resource-id`, `package` or `depth` per node, and the focus answer no `class` or `package`; the daemon read none of them.
- **macOS menu toggles.** A `{menu, on}` step on macOS opens the item's menus, closes them and reads the check mark from a fresh snapshot before it presses: AppKit validates a mark only while its menu is open, so a freshly launched TextEdit read an unchecked item as checked and toggled it twice, on either bundled driver. This fixes the 0.1.6 known issue; a toggle step costs about 2 s more while the app is running, and up to about 4 s more on the first toggle after a launch.

- **Linux docks and shell windows.** The settle's screen hashes skip only the squares a dock window covers, read from the window manager and read again when windows open or close or a dock moves, in place of fixed 32 and 40-row strips at the top and bottom of the screen; `windows` marks a window of a process that owns a dock or desktop window (`_NET_WM_WINDOW_TYPE`) with `shell: true`, and `gui_plan` leaves the shell out by that mark instead of by XFCE's names.
- **Linux row bounds.** AT-SPI table and tree rows on screen carry `bounds` in `snapshot` and `find`, as on the other backends.

## 0.1.6 - 2026-10-19

- **Windows tree items.** On Windows `press` clicks a tree item at its centre when a hit test reaches it, and otherwise tries UI Automation Select, then the item's action: File Explorer's navigation pane on some Windows 11 builds selects an item on Select without opening it.
- **macOS app step.** A macOS desk answers `launch` with `background: true`, after the bundled driver's launch contract, and the app step then brings the launched window to the front, so a plan no longer ends with its app behind Finder and `no focused window` on the next desk call. A focus by title takes the active window among equal titles on every OS, and the run record's app step says how its focus ended (`focused`).
- **`SHUTTLE_SOCKET` everywhere.** The daemon hands its endpoint to `script_run`, `gui_plan`, plugins and services in `SHUTTLE_SOCKET`, the variable the binary already reads, in place of `TOOLHOST_SOCKET`: a plugin or service that read `TOOLHOST_SOCKET` reads `SHUTTLE_SOCKET` now.
- **Runtime without fallbacks.** `desk.wait` calls `desk_wait`, which every desk answers, and no longer polls `windows` when a host lacks it; `gui_plan` keeps flows only where `SHUTTLE_FLOWS` points, which the daemon always sets, and fails without it instead of falling back to `~/workspace/flows` or `/workspace/flows`.
- **`desk shot` text without the image.** On Linux and Android the text of a `shot` no longer repeats the PNG as `png_b64`: the image is in `image_b64` as on macOS and Windows, which halves the frame, and `cocoon-shuttle desk shot` no longer prints the base64 before saving the image.
- **Android taps against system windows.** `computer_state` keeps tap points clear of the system windows over the app, as `press` does:
  - a list row under the navigation bar or the keyboard is tapped in its open part, and one fully covered is left out;
  - an indexed `computer_act` reads the windows again and refuses, with `call computer_state again`, when the window the state came from is no longer in front (the notification shade is open, say), a system window now covers the element's saved point (a keyboard raised since), or the window list is unavailable; explicit `x`,`y` are not checked;
  - the UI server's dump carries its window's id, and `computer_state` retries once when the tree and the front window differ, then answers `the screen is changing`; a dump without an id (an older `ui_jar`) is not compared.
- **Android type.** A type on a device pastes line by line with Return between lines, in `desk type` and `computer_act` alike, and reads each line back from the focused field: a field that did not change is pasted again within 1.5 s; one that changed otherwise (an insert inside its text, a format), a password field and a web view get one unchecked paste; a `computer_act` type whose tap left another field focused fails without a paste; `desk type` answers `typed` true, false or null. scrcpy only sets the clipboard: the UI server presses the paste key and waits until the focused window has handled it, and a window that does not within 5 s fails the type with its name.
- **Bundled:** cua-driver v0.36.0-loomhand.6 (was .5). On macOS a fast relaunch no longer reports an app as already in front while another app is, so the focus after a launch brings it to the front.
- **Known issue:** on macOS a `{menu, on}` toggle whose app was just brought to the front can read its check mark stale and toggle twice; fixed next release.

## 0.1.5 - 2026-10-19

- **v0.1.4 is void:** its binaries report 0.1.3, since its version bump was missing. This release carries v0.1.4's changes, listed here, and the Android fixes below.
- **Linux desktop.** On X11 the daemon drives the desktop itself ([docs/platforms.md](docs/platforms.md#linux)):
  - the desk verbs answer from AT-SPI trees, read in parallel, with input through the bundled cua-driver, which the Linux daemon requires;
  - typing goes through XTest for printable ASCII on a plain keyboard map, and other text goes through the driver;
  - shots read the screen over MIT-SHM, and `windows` names a modal dialog the screen does not show;
  - `close` asks the window manager; launched apps move into systemd user scopes that outlive the daemon, and the launch result says `scoped`;
  - `desk computer_state`, `computer_act`, `computer_end`, `look`, `computer_scale` and `title_at` answer on Linux, refused under an app allowlist;
  - the hotkey, indicator and confirm dialog are not built on Linux yet.
- **Service plugins.** A plugin-directory file with a `service` key names a long-lived program and the tools it serves, each with its own input schema ([docs/wire.md](docs/wire.md#service-plugins)):
  - the daemon starts it with itself in a process group of its own, hands it a socket in a 0700 directory beside its endpoint (`SHUTTLE_SERVICE_SOCKET`), and starts it again with a backoff from 1 s to 30 s when it exits;
  - for a call of one of its tools, the daemon reads the first frame, writes it unchanged to the service's socket and copies bytes both ways until the service closes, with no desk turn and no deadline; about 20 µs added per call at p50 on an Apple M2 Max;
  - `cocoon-shuttle mcp` lists its tools with their schemas, except those with `mcp: false`, and forwards their calls;
  - a stop refuses new calls and closes those in flight, and the service keeps running; `script_run = false` does not turn services off, so the guard rails hold only with no service installed ([docs/safety.md](docs/safety.md#service-plugins));
  - unix only: Windows skips service files with a log line.
- **Android over adb.** With an `[android]` section ([docs/config.md](docs/config.md#android-over-adb)), the daemon drives one Android device instead of the desktop. The device is reached from a Linux or macOS host, and the `shuttle-android` crate holds the backend:
  - the desk verbs answer from the device's UiAutomation tree, through a UI server the daemon pushes and runs under `app_process`;
  - input goes through `adb shell input`, screenshots through `screencap`, and text through a scrcpy clipboard paste;
  - `scroll`, `drag` and `still` answer `unknown desk tool`;
  - the daemon starts no cua-driver, indicator or stop hotkey;
  - `desk computer_state`, `desk computer_act`, `desk computer_end` and `desk look` answer from the command line only; state, act and look write their screenshot to a file, and `computer_end` forgets a session's element indexes;
  - `desk computer_act` is a desk action: `confirm` and the turn apply to it;
  - `launch` takes only package and component names;
  - an `[android]` section with a nonempty `[safety] apps` fails to load, since the device desk enforces no allowlist yet.
  - the screen size comes from `wm size`, read once (restart the daemon after an override), in its natural orientation: a portrait phone turned sideways gets swapped `screen` axes, the portrait default `shot` crop, full-screen windows listed as dialogs, and portrait-sized scrolls and list swipes, while taps and swipes at absolute points pass through; `su` is optional, and without it the paste runs as adb's shell uid, which takes the upstream scrcpy server.
  - an element is tapped in the middle of its part inside the lists that scroll it and the screen, so a row half under an app's own toolbar is tapped where it shows; a row clipped away is off the screen: `computer_state` leaves it out and `press` scrolls to it;
  - the UI server connects UiAutomation with `FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES`, so accessibility services the user enabled stay bound while it runs; before, Android unbound all of them for the whole session.

  The linux archives carry the UI server as `android/cb-ui.jar`, built in the release from `android/ui` with a pinned JDK and checked byte for byte across two builds; an `[android]` section without `ui_jar` takes that jar.
  - a UI server that exits before it prints `ready` is reported with its exit status, the last 4 KiB of its stderr and the last `cb.Ui` abort message in the device's crash log buffer, labelled as possibly from an earlier start (a wrong `ui_jar` gives `ClassNotFoundException: cb.Ui`), or `see adb logcat` when there is none;

  Tested on redroid 16; a stock phone is untested.
- **Shared endpoint.** `[daemon] socket_group` opens the daemon's unix socket to one group's members (mode 0660, owned by the group) on macOS and Linux; unset, it stays 0600. Every member can then use the daemon as its user does ([docs/safety.md](docs/safety.md#a-shared-endpoint)). The Windows pipe stays with its user, and the setting fails the file there.
- **Run errors.** A `script_run` or plugin report that carries `error` now answers `is_error: true`, with the whole report (logs, stderr) as its text, through the daemon and the MCP server alike; before, a failed run looked like a success to the client.
- **Cancel on close.** A client that closes its connection to the daemon cancels the call it carried: a run's process group is killed as on a stop and the turn frees at once, so an interrupted agent no longer leaves a plan holding the turn for up to 110 s. A client must keep its side open until the reply ([docs/wire.md](docs/wire.md#framing)).
- **Android fixes.** A tap lands in the part of an element that its scrolling lists and the screen show, so a row under an app's toolbar or a carousel row inside a list no longer taps the bar over it. Other accessibility services stay bound while the UI server runs.

## 0.1.3 - 2026-10-19

- **Desk verbs.** `scroll` (wheel notches at a point, or scroll keys to the front app window; on Windows a wheel at that window's center), `drag` (from one point to another; both ends pass the app allowlist, and `confirm = "coordinate"` asks first) and `still` (waits until the screen stops changing, ignoring blocks that keep changing, such as a clock or a caret; `timeout_ms` is a polling deadline, at most 5 s).
- **Plugins.** `*_run` tools from spec files in a plugin directory (`tools.d` beside `config.toml`, or `[tools] plugins`), each run on its own program under the `script_run` contract; `script_run = false` turns them off. The schema is in [docs/wire.md](docs/wire.md#plugins).
- **Config.**
  - `kill_hotkey = "off"` registers no global hotkey; `cocoon-shuttle stop` and `resume` still work.
  - `[driver] overlay = false` starts the bundled cua-driver without its agent cursor, for a desktop nobody watches; it has no effect on macOS.
  - `decide_build` pins the builds a `decide` or `verify` answer may come from (`#` matches one digit), and an answer from any other build fails with that build named.
- **Fixes.** On Windows the desk no longer scales window bounds by the display scale factor: above 100% the allowlist's window-at-point check behind every click, scroll and drag at a point, and the screenshot blackout of other apps, used misplaced coordinates.
- **Bundled:** cua-driver v0.36.0-loomhand.5 (was .4). On Windows a desktop click whose window closes or hides says so in its summary ("the window closed or hid"); the Linux A2 popup-key fix ships with it for P3.
- **Speed.** The archives carry the runtime's `.mjs` with its types stripped at package time next to the `.mts` sources, and runs use the `.mjs`: about 50 to 130 ms less per `gui_plan` call and 6 to 20 ms less per `script_run` call on macOS and Windows.

## 0.1.2 - 2026-10-18

- **Fixes.**
  - `install.sh` runs the new program before it replaces the installed one and stops when it does not start.
  - `install.ps1` runs in its own script block, so `irm | iex` leaves no variables or preferences in the session. It refuses before it changes anything while a program from the install runs, takes every path literally (a `SHUTTLE_HOME` with brackets works), and trims a trailing separator from `SHUTTLE_HOME`.
  - `gui_plan`'s outline note tells a backend node cap from a text cut and points to `desk snapshot` or `find`, not to tools shuttle does not have; an outline at exactly the text budget gets no note.
  - `desk focus` fails on a window id that is not a number instead of sending a null id.
- **Speed.** `gui_plan` reuses a window list where no desk action runs before the next read, and the desk skips work whose results it never uses.
- The `gui_plan` run log starts over once it passes 1 MiB.

## 0.1.1 - 2026-10-18

- **Fixes.** The Windows binary links the C runtime statically, so it starts on a Windows without the Visual C++ Redistributable (v0.1.0 exited with STATUS_DLL_NOT_FOUND, 0xC0000135); `install.ps1` fails and keeps the previous install when the new program does not start. CI and the release fail when a Windows executable imports the Visual C++ runtime.

## 0.1.0 - 2026-10-18

The first usable release: shuttle drives the desktop on macOS and Windows.

- **Daemon.** `cocoon-shuttle mcp` starts `cocoon-shuttle serve` on the first tool call other than `status`, which the MCP server answers itself (on macOS from `Cocoon Shuttle.app`, so the permissions go to the app). The daemon runs the bundled cua-driver on a socket of its own, restarts it when it exits and stops it with itself; desk actions run one at a time.
- **Tools.** `desk` (windows, wait, snapshot, find, read, press, act, at, key, type, click, focus, close, launch, shot, screen), `screenshot`, `gui_plan` and `script_run` now run on macOS (AX) and Windows (UI Automation). `gui_plan` runs whole GUI plans, saves passing ones as flows and replays them; `script_run` runs a module on the bundled Node with the desk API. On Linux the `desk` and `screenshot` tools and the desk calls of a plan or script answer that there is no desk backend yet (P3); `script_run` and model calls run.
- **Models, bring your own key.** One OpenAI-compatible endpoint and key, OpenRouter by default: chat completions for `ask`, `ground`, `decide` and `verify` over chat, and the decisions endpoint for `decide` and `verify`. The key comes from `config.toml` or an environment variable; scripts run without it in their environment.
- **Safety controls on macOS and Windows.**
  - `cocoon-shuttle stop` / `resume`, a global kill hotkey (⌃⌥⌘⎋ on macOS, Ctrl+Alt+Shift+Esc on Windows) and the indicator's Stop and Resume. Stop kills the running plan or script and ends the calls in flight; no MCP tool or plan step can resume.
  - An indicator: a menu-bar item on macOS, a tray icon on Windows.
  - Confirm mode: `confirm = "coordinate"` asks before a click at a point, `"every"` before every desk action. While a dialog is open, every other desk call fails.
  - An app allowlist (`[safety] apps`) that checks every desk call by window, element and point, and blacks out other apps in screenshots.
  - `[tools] script_run = false`; a `config.toml` that fails to load fails every tool call except `status`.
  - These are guard rails for tool use, not a sandbox; they hold only with `script_run = false`. [docs/safety.md](docs/safety.md) lists the limits.
- **Speed.** Batched macOS window lists, process-scoped window reads, the menu bar only in snapshots that need it, a plan step that reuses the plan's snapshot, a window wait in the daemon (`desk wait`) instead of polling, a 1 ms cursor glide, and a grounded click that takes at most 3 screen captures.
  - A plan whose last step is a shifted character such as `shift+a`, an arrow key (with or without shift) while a plain window is in front, or typing without a newline ends without the end-of-plan window wait; Tab and shift+Tab keep it.
  - The macOS window list skips the bundled cua-driver's own windows instead of reading them.
- **Fixes.** On Windows a dialog inside its window's tree, such as Notepad's unsaved-changes prompt, shows in `desk snapshot` and `find` and can be pressed; before, the walk skipped it.
- **Bundled:** cua-driver v0.36.0-loomhand.4 and Node.js 24.22.0, both checked by sha256 at build time.
- **Docs:** a first-time setup in [docs/install.md](docs/install.md) (install, macOS permissions, your key, the safety settings to start with), and the status of every platform.

## 0.0.1 - 2026-10-17

- One cargo workspace: `shuttle-desk-core` (the desk wire, call arguments and window matching) and `shuttle-desk` (the macOS AX and Windows UIA desk verbs) from cocoonbox with their history, and the `shuttle` binary.
- `cocoon-shuttle mcp` answers MCP on stdio and lists `gui_plan`, `script_run`, `desk`, `screenshot` and `status`; only `status` runs in this release.
- `cocoon-shuttle serve` and `cocoon-shuttle desk VERB` serve and call the desk verbs on macOS and Windows, a developer preview that needs a running cua-driver daemon.
- The desk runtime (`desk.mts`, `gui_plan.mts`) and its tests, unchanged from cocoonbox apart from test import paths.
- Release archives for macOS (arm64, x64), Windows (x64) and Linux (x64, arm64) with cua-driver v0.35.4-loomhand.2 and Node.js 24.22.0, both checked by sha256 at build time; `SHA256SUMS` and build provenance attestations.
- `install.sh` and `install.ps1`.
