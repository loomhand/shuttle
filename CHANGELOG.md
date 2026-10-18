# Changelog

## Unreleased

- **Android over adb.** With an `[android]` section ([docs/config.md](docs/config.md#android-over-adb)), the daemon drives one Android device instead of the desktop. The device is reached from a Linux or macOS host, and the `shuttle-android` crate holds the backend:
  - the desk verbs answer from the device's UiAutomation tree, through a UI server the daemon pushes and runs under `app_process`;
  - input goes through `adb shell input`, screenshots through `screencap`, and text through a scrcpy clipboard paste;
  - `scroll`, `drag` and `still` answer `unknown desk tool`;
  - the daemon starts no cua-driver, indicator or stop hotkey;
  - `desk computer_state`, `desk computer_act` and `desk look` answer from the command line only, with their screenshot written to a file;
  - `desk computer_act` is a desk action: `confirm` and the turn apply to it;
  - `launch` takes only package and component names;
  - an `[android]` section with a nonempty `[safety] apps` fails to load, since the device desk enforces no allowlist yet.

  Tested on redroid 16; a stock phone is untested.

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
