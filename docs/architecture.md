# Architecture

## Processes

```
agent (Claude Code, Codex, Cursor)
  │ MCP, JSON-RPC over stdio
  ▼
shuttle mcp ───────── one per agent session; a thin proxy
  │ desk wire (u32 length + JSON) over a 0600 unix socket or a local-only named pipe
  ▼
shuttle serve ─────── one per user; owns the desktop, the model keys and the safety controls
  ├── desk verbs ─────────── AX (macOS), UI Automation (Windows), AT-SPI (Linux)
  ├── cua-driver ─────────── the bundled driver, a child on the daemon's own socket or pipe: input, window focus, launch, capture
  ├── node ───────────────── child process per gui_plan / script_run, running the runtime;
  │                          desk.mts (desk.mjs in the archives) calls back over the same desk wire
  └── service plugins ────── long-lived children from the plugin directory, none by default; the daemon
                             splices each call of a service tool to the service's own socket
```

- **`shuttle mcp`** speaks MCP to the agent and forwards the `desk` and `screenshot` tools to the daemon, starting it when nothing listens (from an app bundle through `open -n -g -a`, a new instance, since a bundled cua-driver already counts as the running app; else as a detached `serve`), except with `SHUTTLE_EMBEDDED=1`, where the app that embeds shuttle runs `serve` and `mcp` only connects ([install.md](install.md#embedding-shuttle-in-an-app)). A call that may have reached the daemon is never sent twice. `gui_plan` and `script_run` go there too: the daemon runs them on the bundled Node with `SHUTTLE_SOCKET` naming its endpoint, so the runtime's desk and model calls come back to it.
- **`shuttle serve`** is the one process that touches the desktop and holds the model keys: it answers `desk_*` with this OS's desk and `model_*` with the providers of [config.md](config.md). Several agents share it; desk actions (press, act, key, type, click, focus, close, launch) run one at a time, so two agents never type into the same window at once, while reads run alongside them; a `gui_plan` or `script_run` owns the desk until it ends, so only its own actions run meanwhile; an action or run that waits more than 15 s fails with `desk busy` instead of overlapping.
- **macOS** attributes Accessibility and Screen Recording grants to an app bundle, not to a bare binary, so the daemon runs from `Shuttle.app` (`open -n -g -a ~/Applications/Loomhand/Shuttle.app --args serve`, by path, since another app may also be named Shuttle) and node runs as its child inside the same responsibility chain. A daemon started by a terminal would borrow the terminal's grants instead. The bundled cua-driver lives in the same bundle, so macOS attributes it to Shuttle too: one set of grants covers the daemon and the driver.
- **Node** is the official Node.js 24 binary, shipped unmodified and checked by sha256 at build time. A single-file compile (bun `--compile`, Node SEA) does not fit: `script_run` loads arbitrary scripts at run time, and the runtime must match what its tests run on.
- **Service plugins** ([wire.md](wire.md#service-plugins)) are programs the plugin directory names. The daemon starts each with itself and starts it again when it exits. For a call of one of its tools, the daemon reads the first frame and copies the connection both ways to the service's socket, with no desk turn, so a service's calls run alongside plans and desk actions. A desktop install ships none.
- **Model calls** (`decide`, `ground`, `ask`, `verify`) go through the daemon as `model_*` desk-wire calls, so provider keys live in the daemon and stay out of a script's environment; see [config.md](config.md#keys).

## What shuttle never does

shuttle is thin and embeddable: it sends each model call once, non-streaming, to the one OpenAI-compatible endpoint and the decisions URL it is configured with. It never speaks a vendor's native API, streams, caches, retries or fails over across vendors, routes between models, or bills. Anyone who needs those points `base_url` and `decisions_url` at a gateway that does them. Beyond that configured endpoint, shuttle depends on no hosted service.

## Components

| Path | Role |
|---|---|
| `crates/desk-core` (`shuttle-desk-core`, lib `desk_core`) | The desk wire, the desk call arguments and the pure window and name matching every desk backend shares |
| `crates/desk` (`shuttle-desk`) | The desk verbs over the macOS AX, Windows UIA and Linux AT-SPI trees, the cua-driver client, and on Linux XTest typing, MIT-SHM capture and launch scopes |
| `crates/android` (`shuttle-android`) | The desk verbs on an Android device over adb: the UI server's element trees and screenshots, input (taps and single keys), `screencap` as the fallback and a scrcpy paste |
| `android/ui` | The UI server the Android desk pushes to the device and runs under `app_process`, built into `shuttle-ui.jar` |
| `crates/models` (`shuttle-models`) | The model calls: chat completions and decisions over one OpenAI-compatible endpoint, a model name per role; usable as a library with a `Config` value |
| `crates/shuttle` | The `shuttle` binary: the MCP proxy (`mcp`), the daemon with its script runner and service plugins (`serve`), `desk VERB`, `stop` and `resume`, and the safety controls (kill hotkey, indicator, confirm dialog) |
| `runtime/lib` | `desk.mts` (the desk API for scripts) and `gui_plan.mts` (the plan executor, flows and replays) |
| `runtime/test` | Node tests that drive the runtime against fake desk sockets and fake model servers |
| `packaging` | The release archive builder, the runtime type stripper, the third-party pins, the macOS bundle template and the installers |
