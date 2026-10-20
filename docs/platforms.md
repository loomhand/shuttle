# Platforms

| | macOS | Windows | Linux |
|---|---|---|---|
| Release archive | arm64, x64 (13.5+) | x64 | x64, arm64 (glibc 2.31+) |
| Accessibility tree | AX: done in `shuttle-desk` | UI Automation: done in `shuttle-desk` | AT-SPI: done in `shuttle-desk` |
| Input, windows, capture | cua-driver | cua-driver | cua-driver, with XTest typing, MIT-SHM capture and window-manager close (X11 only) |
| MCP tools that act | built (P1) | built (P2) | built (P3) |
| Kill switch, indicator, allowlist, confirm | built (P1) | built (P2) | `stop`/`resume`, the `script_run` switch and the allowlist built; no hotkey, indicator or confirm dialog |

Every platform installs and runs `cocoon-shuttle mcp`. On macOS and Windows, the `desk`, `screenshot`, `gui_plan` and `script_run` tools reach the desk verbs through the daemon, which runs the bundled cua-driver, with the safety controls of [safety.md](safety.md). On Linux (X11) the same tools reach the desk verbs over AT-SPI, with input through the bundled cua-driver; the daemon needs the desktop session's environment (`DISPLAY`, `XAUTHORITY`, the session D-Bus), which it has when an MCP client in that session starts it.

## macOS

- **Permissions.** shuttle needs Accessibility (to read and act on windows) and Screen Recording (for screenshots), and on macOS 26 the screen-capture consent cua-driver asks for. macOS grants them to an app, so they go to `Cocoon Shuttle.app`, which the installer puts in `~/Applications`; the bundled cua-driver runs from the same bundle, so the same grants cover it. Grant them in System Settings > Privacy & Security when shuttle first asks.
- **cua-driver.** The daemon runs the bundled cua-driver on a socket of its own, `driver.sock` beside `desk.sock` (`\\.\pipe\cocoon-shuttle-desk-driver` on Windows), with the driver's telemetry off (`CUA_DRIVER_RS_TELEMETRY_ENABLED=false`). It starts the driver again when it exits and stops it when the daemon exits; a driver that does not listen within 10 s stops the daemon from starting. A cua-driver you run yourself at its default socket is not used; a build without a bundled driver, such as `cargo run`, uses that one.
- **No code signing yet.** `Cocoon Shuttle.app` carries an ad hoc signature. macOS ties the grants to that signature, which changes with every release and every local build, so grant the permissions again after each upgrade. The old entries stay listed but no longer match: clear them with `tccutil reset Accessibility io.github.loomhand.cocoon-shuttle` and `tccutil reset ScreenCapture io.github.loomhand.cocoon-shuttle`, quit the daemon (`pkill -f 'cocoon-shuttle serve'`), and grant again when shuttle asks.
- **Displays.** Setups that mix display scales are untested: accessibility bounds use the main display's scale, so on a display with another scale a control may be missed or clicked off target.
- **Gatekeeper.** The installer downloads with `curl`, which sets no quarantine flag. A copy downloaded with a browser is quarantined: run `xattr -dr com.apple.quarantine ~/Applications/"Cocoon Shuttle.app"`, or open it once with Control-click > Open.
- **Input.** Every menu-item action runs the item's AX action, also when the item has a keyboard shortcut: a `gui_plan` menu step, a click on a menu item and a press. Any other press clicks the element's centre when a hit test there finds it, else runs its AX action, and keys go to the desktop through cua-driver. A click on an open menu item runs the item through accessibility. Any other click brings the top window under the point to the front and clicks it in the pixels of the driver's screenshot of that window without moving the pointer, so the keys that follow reach it; a window whose pointer input cua-driver refuses, such as a sheet, and a point over no window take a desktop click, which cua-driver refuses over another app; [safety.md](safety.md#input-on-macos) describes the order shuttle tries.

## Windows

- **SmartScreen.** The binaries are unsigned. `install.ps1` downloads without the mark of the web; a zip downloaded with a browser may still show "Windows protected your PC" (More info > Run anyway).
- **Elevated windows.** Driving a window that runs as administrator needs a UIAccess helper, which Windows starts only when it is signed and installed under Program Files. shuttle ships without it, so windows of elevated processes are out of reach.
- **Windows on Arm** runs the x64 build under emulation.

## Linux

- **X11:** AT-SPI gives the trees, read in parallel over D-Bus. Typing goes through XTest on the core keyboard for a line of printable ASCII and tabs, when the keyboard map holds every character on its first group with no Caps or Shift lock; any other text, such as accented or CJK characters, and every other input, goes through the bundled cua-driver, one driver session per call. Shots read the screen over MIT-SHM, else the driver's capture, and `windows` reports a modal dialog the screen does not show as `modal` and `blocks`, and marks the windows of the panel, dock and desktop processes as `shell`. `close` asks the window manager (`_NET_CLOSE_WINDOW`); it fails when the window manager does not support that request or two windows of the process share the title. The computer calls ([wire.md](wire.md#computer-calls)) run here only: element lists from the driver's window state, screenshots over MIT-SHM scaled and encoded as WebP or JPEG, and after each action a wait until the screen is still that ignores the docks and blocks such as a clock or a caret, which a sampler learns while the calls are in use.
- **Launched apps** move into a systemd user scope of their own (`app-desk-APP-PID.scope`) when a systemd user manager runs, so a restart of the daemon or its service does not end them. The move is best effort: `scoped: true` means the request to systemd went out, before its start job finished, and a stop that lands between the driver's launch reply and the move leaves the app where it was. Without a user manager, the launch result says `scoped: false`: the app stays in the daemon's cgroup, and a restart of the daemon or its service can end it.
- **X11 is required.** The desk reads the screen, types and closes windows through the X server; a Wayland-only session is not supported. Under XWayland, X11 apps work as far as the compositor lets an X client see them.
- **Known limits.** Two windows of one process with the same title both show the active state when one of them is active. A modifier that Sticky Keys latched changes what XTest types. A key XTest pressed stays down if the X connection dies mid-line.
- **AT-SPI** must be on: most desktops start it; on a minimal session, start `at-spi-bus-launcher` and set `GTK_MODULES=gail:atk-bridge` for older GTK apps.

## Android over adb

- **adb mode.** The daemon drives one Android device that adb reaches, from a Linux or macOS host, when the configuration has an `[android]` section ([config.md](config.md#android-over-adb)): the device's UiAutomation tree through a UI server it pushes and runs under `app_process`, taps and single keys injected by the UI server, other input through `adb shell input`, screenshots from the UI server (`screencap` as the fallback), and text through a scrcpy clipboard paste. The `shuttle-android` crate holds it.
- **Tested** on redroid 16 in a container, at 720x1280, with adb root and also without it: without root the UI server and the upstream scrcpy server's paste run as the shell uid, as on a stock phone. A stock phone itself is untested.
- **One accessibility client.** Android lets one UiAutomation client run at a time, so `uiautomator dump` or a second shuttle on the same device stops the UI server; the next desk call starts it again.
