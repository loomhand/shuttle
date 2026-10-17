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

- **`cocoon-shuttle mcp`** speaks MCP to the agent and forwards each tool call to the daemon. It starts the daemon when none answers. In v0.0.1 it answers on its own and runs only `status`.
- **`cocoon-shuttle serve`** is the one process that touches the desktop. Several agents share it; one actor lock serializes their actions, so two agents never type into the same window at once.
- **macOS** attributes Accessibility and Screen Recording grants to an app bundle, not to a bare binary, so the daemon runs from `Cocoon Shuttle.app` (`open -g -a "Cocoon Shuttle" --args serve`) and cua-driver and node run as its children inside the same responsibility chain. A daemon started by a terminal would borrow the terminal's grants instead.
- **Node** is the official Node.js 24 binary, shipped unmodified and checked by sha256 at build time. A single-file compile (bun `--compile`, Node SEA) does not fit: `script_run` loads arbitrary scripts at run time, and the runtime must match what its tests run on.
- **Model calls** (`decide`, `ground`, `ask`, `verify`) go through the daemon as `model_*` desk-wire calls, so provider keys live only in the daemon and never reach a script; see [config.md](config.md).

## Components

| Path | Role |
|---|---|
| `crates/desk-core` (`shuttle-desk-core`, lib `desk_core`) | The desk wire, the desk call arguments and the pure window and name matching every desk backend shares |
| `crates/desk` (`shuttle-desk`) | The desk verbs over the macOS AX and Windows UIA trees and the cua-driver client; the Linux AT-SPI backend joins in P3 |
| `crates/shuttle` | The `cocoon-shuttle` binary: `mcp`, `serve`, `desk VERB`; later the daemon, the model providers and the safety controls |
| `runtime/lib` | `desk.mts` (the desk API for scripts) and `gui_plan.mts` (the plan executor, flows and replays) |
| `runtime/test` | Node tests that drive the runtime against fake desk sockets and fake model servers |
| `packaging` | The release archive builder, the third-party pins, the macOS bundle template and the installers |

## Dependency plan with cocoonbox

shuttle owns the desk: `desk-core`, the desk backends and the runtime. cocoonbox, which drives cloud desktops, will depend on shuttle and not the reverse:

- cocoonbox takes `shuttle-desk-core` and `shuttle-desk` as git dependencies pinned to a shuttle tag, and the runtime files from that tag by sha256 when it cuts an image.
- Its in-box toolhost answers the `model_*` calls by forwarding them to its gateway, so the same `desk.mts` runs in both.
- The Linux AT-SPI backend moves from cocoonbox's toolhost into `shuttle-desk` (P3), and cocoonbox's own copies of `desk-core`, `deskhost` and the runtime go away (P4).

shuttle stays usable on its own: nothing in it names a cloud service or needs one.
