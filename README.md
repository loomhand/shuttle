# shuttle

shuttle (binary `cocoon-shuttle`, app Cocoon Shuttle) is a computer-use host that runs on your own machine. Local agents such as Claude Code, Codex and Cursor start it as an MCP server over stdio and drive your desktop through it: whole GUI plans in one call (`gui_plan`), scripts against a desk API (`script_run`), single accessibility-tree calls (`desk`) and screenshots. Model calls for target picking, grounding and checks go to endpoints and keys you configure (bring your own key).

shuttle is part of [loomhand](https://github.com/loomhand). It takes its desk core and plan executor from [cocoonbox](https://github.com/loomhand/cocoonbox), where they drive cloud desktops, and its input, capture and window layer from [cua-driver](https://github.com/loomhand/cua) (MIT).

**Documentation: [loomhand.github.io/shuttle](https://loomhand.github.io/shuttle/)** (source in [`docs/`](docs/)).

## Status

**v0.0.1 is a skeleton.** It installs, answers MCP on stdio and lists its five tools; only `status` runs. The tools that act on the desktop land per platform:

| Phase | What | State |
|---|---|---|
| P0 | Workspace, CI, release archives, installers, MCP skeleton | done in v0.0.1 |
| P1 | macOS: daemon in `Cocoon Shuttle.app`, desk tools, `gui_plan`, `script_run`, BYOK models, safety controls | next |
| P2 | Windows: the same over UI Automation | planned |
| P3 | Linux: the same over AT-SPI on X11 | planned |
| P4 | cocoonbox depends on shuttle's crates and runtime | planned |

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

The installers check the archive against the release's `SHA256SUMS`. The binaries are not code-signed; [docs/install.md](docs/install.md) covers macOS Gatekeeper and permissions, Windows SmartScreen, manual installs, provenance checks and uninstalling.

## Use it from an agent

```sh
claude mcp add shuttle -- cocoon-shuttle mcp
```

Codex (`~/.codex/config.toml`):

```toml
[mcp_servers.shuttle]
command = "cocoon-shuttle"
args = ["mcp"]
```

Cursor (`~/.cursor/mcp.json`):

```json
{ "mcpServers": { "shuttle": { "command": "cocoon-shuttle", "args": ["mcp"] } } }
```

## Tools

| Tool | What it does |
|---|---|
| `gui_plan` | Runs a GUI plan in one call: each step (`app`, `menu`, `click`, `check`, `set`, `type`, `key`, `wait`, `verify`, `close`) resolves on the accessibility tree, is checked, and the plan stops at the first surprise; a passing plan can be saved as a flow and replayed |
| `script_run` | Runs one JavaScript or TypeScript module on the bundled Node runtime with the desk API |
| `desk` | One desk call: windows, snapshot, find, read, press, act, at, key, type, click, focus, close, launch, shot, screen |
| `screenshot` | A PNG of the screen or a rectangle |
| `status` | Version, platform, and which tools this release runs |

## Safety

shuttle drives your real machine. From P1 it has a global kill-switch hotkey (macOS ⌃⌥⌘⎋, Windows and Linux Ctrl+Alt+Shift+Esc), a visible indicator while it acts, an optional per-step confirm mode and an app allowlist. `script_run` executes code with your user's rights; the allowlist bounds desk calls, not that code, and `script_run` can be turned off. [docs/safety.md](docs/safety.md) also describes how input works on macOS, where there is only one pointer.

## Documentation

- [docs/architecture.md](docs/architecture.md): processes, components and the dependency plan with cocoonbox
- [docs/config.md](docs/config.md): the model configuration (bring your own key)
- [docs/safety.md](docs/safety.md): kill switch, indicator, confirm mode, allowlist, input per OS
- [docs/platforms.md](docs/platforms.md): what exists per OS and the permissions each needs
- [docs/install.md](docs/install.md): installers, verification, manual install, uninstall
- [docs/wire.md](docs/wire.md): the desk wire between the host, the runtime and the desk backends
- [docs/development.md](docs/development.md): building, testing and releasing

## License

[AGPL-3.0-only](LICENSE). Release archives also carry cua-driver (MIT) and Node.js (MIT) with their licenses.
