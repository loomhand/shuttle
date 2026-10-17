# Platforms

| | macOS | Windows | Linux |
|---|---|---|---|
| Release archive | arm64, x64 (13.0+) | x64 | x64, arm64 (glibc 2.31+) |
| Accessibility tree | AX: done in `shuttle-desk` | UI Automation: done in `shuttle-desk` | AT-SPI: in cocoonbox's toolhost, moves here in P3 |
| Input, windows, capture | cua-driver | cua-driver | cua-driver (X11; Wayland best effort) |
| MCP tools that act | P1 | P2 | P3 |
| Kill switch, indicator, allowlist, confirm | P1 | P2 | P3 |

In v0.0.1 every platform installs, runs `shuttle mcp` and answers `status`. On macOS and Windows, `shuttle serve` and `shuttle desk VERB` already serve and call the desk verbs when a cua-driver daemon runs at its default endpoint; that is a developer preview without the safety controls.

## macOS

- **Permissions.** shuttle needs Accessibility (to read and act on windows) and Screen Recording (for screenshots). macOS grants them to an app, so they go to `Shuttle.app`, which the installer puts in `~/Applications`. Grant them in System Settings > Privacy & Security when shuttle first asks.
- **No code signing yet.** `Shuttle.app` carries an ad hoc signature. macOS ties the grants to that signature, which changes with every release, so grant the permissions again after each upgrade.
- **Gatekeeper.** The installer downloads with `curl`, which sets no quarantine flag, and clears it anyway. A copy downloaded with a browser is quarantined: run `xattr -dr com.apple.quarantine ~/Applications/Shuttle.app`, or open it once with Control-click > Open.
- **Input.** See [safety.md](safety.md#input-on-macos): AX actions first, keys to the app's process, coordinate clicks last.

## Windows

- **SmartScreen.** The binaries are unsigned. `install.ps1` downloads without the mark of the web and runs `Unblock-File`; a zip downloaded with a browser may still show "Windows protected your PC" (More info > Run anyway).
- **Elevated windows.** Driving a window that runs as administrator needs a UIAccess helper, which Windows starts only when it is signed and installed under Program Files. shuttle ships without it, so windows of elevated processes are out of reach.
- **Windows on Arm** runs the x64 build under emulation.

## Linux

- **X11** is the target for P3: AT-SPI for the tree, XTest through cua-driver for input.
- **Wayland** gives AT-SPI trees but no global input or window positions to a client. cua-driver goes through the desktop portal (with its consent prompts) and, on GNOME, a small shell extension in `wayland-helper/` for window positions. Expect gaps.
- **AT-SPI** must be on: most desktops start it; on a minimal session, start `at-spi-bus-launcher` and set `GTK_MODULES=gail:atk-bridge` for older GTK apps.
