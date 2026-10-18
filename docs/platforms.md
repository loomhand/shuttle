# Platforms

| | macOS | Windows | Linux |
|---|---|---|---|
| Release archive | arm64, x64 (13.5+) | x64 | x64, arm64 (glibc 2.31+) |
| Accessibility tree | AX: done in `shuttle-desk` | UI Automation: done in `shuttle-desk` | AT-SPI: in cocoonbox's toolhost, moves here in P3 |
| Input, windows, capture | cua-driver | cua-driver | cua-driver (X11; Wayland best effort) |
| MCP tools that act | developer preview on `main` (P1) | developer preview on `main` (P2) | P3 |
| Kill switch, indicator, allowlist, confirm | built (P1) | built (P2) | P3 |

Every platform installs and runs `cocoon-shuttle mcp`. On macOS and Windows, the `desk`, `screenshot`, `gui_plan` and `script_run` tools reach the desk verbs through the daemon, which runs the bundled cua-driver; that is a developer preview without the safety controls. On Linux, `gui_plan` and `script_run` run, but their desk calls answer that there is no desk backend yet.

## macOS

- **Permissions.** shuttle needs Accessibility (to read and act on windows) and Screen Recording (for screenshots), and on macOS 26 the screen-capture consent cua-driver asks for. macOS grants them to an app, so they go to `Cocoon Shuttle.app`, which the installer puts in `~/Applications`; the bundled cua-driver runs from the same bundle, so the same grants cover it. Grant them in System Settings > Privacy & Security when shuttle first asks.
- **cua-driver.** The daemon runs the bundled cua-driver on a socket of its own, `driver.sock` beside `desk.sock` (`\\.\pipe\cocoon-shuttle-desk-driver` on Windows), with the driver's telemetry off (`CUA_DRIVER_RS_TELEMETRY_ENABLED=false`). It starts the driver again when it exits and stops it when the daemon exits; a driver that does not listen within 10 s stops the daemon from starting. A cua-driver you run yourself at its default socket is not used; a build without a bundled driver, such as `cargo run`, uses that one.
- **No code signing yet.** `Cocoon Shuttle.app` carries an ad hoc signature. macOS ties the grants to that signature, which changes with every release and every local build, so grant the permissions again after each upgrade. The old entries stay listed but no longer match: clear them with `tccutil reset Accessibility io.github.loomhand.cocoon-shuttle` and `tccutil reset ScreenCapture io.github.loomhand.cocoon-shuttle`, quit the daemon (`pkill -f 'cocoon-shuttle serve'`), and grant again when shuttle asks.
- **Gatekeeper.** The installer downloads with `curl`, which sets no quarantine flag. A copy downloaded with a browser is quarantined: run `xattr -dr com.apple.quarantine ~/Applications/"Cocoon Shuttle.app"`, or open it once with Control-click > Open.
- **Input.** Every menu-item action runs the item's AX action, also when the item has a keyboard shortcut: a `gui_plan` menu step, a click on a menu item and a press. Any other press clicks the element's centre when a hit test there finds it, else runs its AX action, and keys go to the desktop through cua-driver. A click on an open menu item runs the item through accessibility. Any other click brings the top window under the point to the front and clicks it in the pixels of the driver's screenshot of that window without moving the pointer, so the keys that follow reach it; a window whose pointer input cua-driver refuses, such as a sheet, and a point over no window take a desktop click, which cua-driver refuses over another app; [safety.md](safety.md#input-on-macos) describes the P1 design.

## Windows

- **SmartScreen.** The binaries are unsigned. `install.ps1` downloads without the mark of the web; a zip downloaded with a browser may still show "Windows protected your PC" (More info > Run anyway).
- **Elevated windows.** Driving a window that runs as administrator needs a UIAccess helper, which Windows starts only when it is signed and installed under Program Files. shuttle ships without it, so windows of elevated processes are out of reach.
- **Windows on Arm** runs the x64 build under emulation.

## Linux

- **X11** is the target for P3: AT-SPI for the tree, XTest through cua-driver for input.
- **Wayland** gives AT-SPI trees but no global input or window positions to a client. cua-driver goes through the desktop portal (with its consent prompts) and, on GNOME, a small shell extension in `wayland-helper/` for window positions. Expect gaps.
- **AT-SPI** must be on: most desktops start it; on a minimal session, start `at-spi-bus-launcher` and set `GTK_MODULES=gail:atk-bridge` for older GTK apps.
