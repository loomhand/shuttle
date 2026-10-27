# shuttle

shuttle (binary `shuttle`, app Shuttle) is a computer-use host that runs on your own machine. Local agents such as Claude Code, Codex and Cursor start it as an MCP server over stdio and drive your desktop through it: whole GUI plans in one call (`gui_plan`), scripts against a desk API (`script_run`), single accessibility-tree calls (`desk`) and screenshots. Model calls for target picking, grounding and checks go to an OpenAI-compatible endpoint and key you configure, OpenRouter by default (bring your own key).

shuttle is part of [loomhand](https://github.com/loomhand). Its input, capture and window layer is [cua-driver](https://github.com/loomhand/cua) (MIT).

**Documentation: [loomhand.github.io/shuttle](https://loomhand.github.io/shuttle/)** (source in [`docs/`](docs/)).

## Status

**v0.1.25 is the current release.** On macOS and Windows the daemon drives the desktop through the bundled cua-driver (`desk`, `screenshot`, `gui_plan`, `script_run`), calls models with your own key, and has its safety controls: kill switch and hotkey, indicator, confirm mode and app allowlist. On Linux (X11) the daemon drives the desktop over AT-SPI, with input through the bundled cua-driver, the stop, the `script_run` switch and the app allowlist; its hotkey, indicator and confirm dialog are not built yet. On Linux and macOS the daemon can drive an Android device over adb instead of the desktop ([Android over adb](docs/platforms.md#android-over-adb)). v0.1.0's Windows build needs the Visual C++ Redistributable; later releases do not.

| Phase | What | State |
|---|---|---|
| P0 | Workspace, CI, release archives, installers, MCP skeleton | done in v0.0.1 |
| P1 | macOS: daemon in `Shuttle.app`, desk tools, `gui_plan`, `script_run`, BYOK models, safety controls | done in v0.1.0 |
| P2 | Windows: the same over UI Automation, with its safety controls | done in v0.1.0; starts without the Visual C++ Redistributable from v0.1.1 |
| P3 | Linux: the same over AT-SPI on X11 | done in v0.1.5, without the kill hotkey, indicator and confirm dialog |

[docs/platforms.md](docs/platforms.md) has the detail per OS.

## Install

macOS and Linux:

```sh
curl -fsSL https://github.com/loomhand/shuttle/releases/latest/download/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://github.com/loomhand/shuttle/releases/latest/download/install.ps1 | iex
```

The installers check the archive against the release's `SHA256SUMS`. The binaries are not code-signed; [docs/install.md](docs/install.md) walks through a first-time setup (permissions, your key, the safety settings to start with) and covers manual installs, provenance checks and uninstalling; [docs/platforms.md](docs/platforms.md) covers macOS Gatekeeper and Windows SmartScreen.

## Use it from an agent

```sh
claude mcp add shuttle -- shuttle mcp
```

Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.shuttle]
command = "shuttle"
args = ["mcp"]
```

Cursor (`~/.cursor/mcp.json`):

```json
{ "mcpServers": { "shuttle": { "command": "shuttle", "args": ["mcp"] } } }
```

## Tools

| Tool | What it does |
|---|---|
| `gui_plan` | Runs a GUI plan in one call: each step (`app`, `menu`, `click`, `check`, `set`, `type`, `key`, `wait`, `expect`, `verify`, `close`) resolves on the accessibility tree, is checked, and the plan stops at the first surprise; a passing plan can be saved as a flow and replayed |
| `script_run` | Runs one JavaScript or TypeScript module on the bundled Node runtime with the desk API |
| `desk` | One desk call: windows, wait, snapshot, find, read, press, act, at, key, type, click, scroll, drag, still, focus, close, launch, shot, screen |
| `screenshot` | A PNG of the screen or a rectangle |
| `status` | Name, version, OS and architecture |

## Safety

shuttle drives your real machine. On macOS and Windows it has `shuttle stop`, a global kill hotkey (macOS ⌃⌥⌘⎋, Windows Ctrl+Alt+Shift+Esc), a menu-bar item or tray icon that shows it acting or stopped, an optional confirm mode that asks before a click at a point or before every action, and an app allowlist that also blacks out other apps in screenshots. These are guard rails for tool use, not a sandbox: `script_run`, which is on by default, runs code with your user's rights, so they hold only with `script_run = false` under `[tools]`. Linux has `stop`, `resume` and the `script_run` switch now, and not yet the hotkey, indicator and confirm mode. [docs/safety.md](docs/safety.md) has the detail and the limits, and how input works on macOS, where there is only one pointer.

## Documentation

- [docs/architecture.md](docs/architecture.md): processes and components
- [docs/config.md](docs/config.md): the model configuration (bring your own key) and the safety settings
- [docs/safety.md](docs/safety.md): kill switch, indicator, confirm mode, allowlist, input per OS
- [docs/platforms.md](docs/platforms.md): what exists per OS and the permissions each needs
- [docs/install.md](docs/install.md): installers, verification, manual install, uninstall
- [docs/wire.md](docs/wire.md): the desk wire between the host, the runtime and the desk backends
- [docs/development.md](docs/development.md): building, testing and releasing

## License

[AGPL-3.0-only](LICENSE). Release archives also carry cua-driver (MIT) and Node.js (MIT) with their licenses.
