# Architecture

## Processes

```
agent (Claude Code, Codex, Cursor)
  │ MCP, JSON-RPC over stdio
  ▼
cocoon-shuttle mcp ───────── one per agent session; a thin proxy
  │ desk wire (u32 length + JSON) over a 0600 unix socket or a local-only named pipe
  ▼
cocoon-shuttle serve ─────── one per user; owns the desktop, the model keys and the safety controls
  ├── desk verbs ─────────── AX (macOS), UI Automation (Windows), AT-SPI (Linux)
  ├── cua-driver ─────────── child process: input, windows, launch, capture, cursor overlay
  └── node ───────────────── child process per gui_plan / script_run, running runtime/*.mts;
                             desk.mts calls back over the same desk wire
```

- **`cocoon-shuttle mcp`** speaks MCP to the agent and forwards the `desk` and `screenshot` tools to the daemon, starting it when nothing listens (from an app bundle through `open -g -a`, else as a detached `serve`). A call that may have reached the daemon is never sent twice. `gui_plan` and `script_run` join with the runtime switch.
- **`cocoon-shuttle serve`** is the one process that touches the desktop and holds the model keys: it answers `desk_*` with this OS's desk and `model_*` with the providers of [config.md](config.md). Several agents share it; desk actions (press, act, key, type, click, focus, close, launch) run one at a time, so two agents never type into the same window at once, while reads run alongside them.
- **macOS** attributes Accessibility and Screen Recording grants to an app bundle, not to a bare binary, so the daemon runs from `Cocoon Shuttle.app` (`open -g -a "Cocoon Shuttle" --args serve`) and cua-driver and node run as its children inside the same responsibility chain. A daemon started by a terminal would borrow the terminal's grants instead.
- **Node** is the official Node.js 24 binary, shipped unmodified and checked by sha256 at build time. A single-file compile (bun `--compile`, Node SEA) does not fit: `script_run` loads arbitrary scripts at run time, and the runtime must match what its tests run on.
- **Model calls** (`decide`, `ground`, `ask`, `verify`) go through the daemon as `model_*` desk-wire calls, so provider keys live only in the daemon and never reach a script; see [config.md](config.md).

## What shuttle never does

shuttle is thin and embeddable: it sends each model call once, non-streaming, to the one OpenAI-compatible endpoint and the decisions URL it is configured with. It never speaks a vendor's native API, streams, caches, retries or fails over across vendors, routes between models, or bills. Anyone who needs those points `base_url` and `decisions_url` at a gateway that does them; cocoonbox embeds shuttle that way, pointed at its own gateway.

## Components

| Path | Role |
|---|---|
| `crates/desk-core` (`shuttle-desk-core`, lib `desk_core`) | The desk wire, the desk call arguments and the pure window and name matching every desk backend shares |
| `crates/desk` (`shuttle-desk`) | The desk verbs over the macOS AX and Windows UIA trees and the cua-driver client; the Linux AT-SPI backend joins in P3 |
| `crates/models` (`shuttle-models`) | The model calls: chat completions and decisions over one OpenAI-compatible endpoint, a model name per role; usable as a library with a `Config` value |
| `crates/shuttle` | The `cocoon-shuttle` binary: `mcp`, `serve`, `desk VERB`; later the daemon and the safety controls |
| `runtime/lib` | `desk.mts` (the desk API for scripts) and `gui_plan.mts` (the plan executor, flows and replays) |
| `runtime/test` | Node tests that drive the runtime against fake desk sockets and fake model servers |
| `packaging` | The release archive builder, the third-party pins, the macOS bundle template and the installers |

## Dependency plan with cocoonbox

shuttle owns the desk: `desk-core`, the desk backends and the runtime. cocoonbox, which drives cloud desktops, will depend on shuttle and not the reverse:

- cocoonbox takes `shuttle-desk-core` and `shuttle-desk` as git dependencies pinned to a shuttle tag, and the runtime files from that tag by sha256 when it cuts an image.
- Its in-box toolhost answers the `model_*` calls by forwarding them to its gateway, so the same `desk.mts` runs in both.
- The Linux AT-SPI backend moves from cocoonbox's toolhost into `shuttle-desk` (P3), and cocoonbox's own copies of `desk-core`, `deskhost` and the runtime go away (P4).

shuttle stays usable on its own: nothing in it names a cloud service or needs one.
