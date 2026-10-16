# cocoonbox-desk

The desk host of a Windows or macOS box. It serves the Linux toolhost's desk calls (`desk_windows`, `desk_snapshot`, `desk_find`, `desk_read`, `desk_press`, `desk_act`, `desk_at`, `desk_key`, `desk_type`, `desk_click`, `desk_focus`, `desk_close`, `desk_launch`, `desk_shot`, `desk_screen`) with the same framing and JSON, so `desk.mts`, `gui_plan.mts` and the `desk` CLI run unchanged. It imports nothing from `agent/`; the contract is the wire format in `docs/contracts.md` (Toolhost wire), and both hosts take it, the desk call arguments and the window and name matching from `desk-core/`.

- `cocoonbox-desk serve [ENDPOINT]` serves one call per connection: a big-endian u32 length and JSON each way. The endpoint is `TOOLHOST_SOCKET`, else `~/Library/Caches/cocoonbox/desk.sock` (mode 0600) on macOS and `\\.\pipe\cocoonbox-desk` (local clients only) on Windows.
- `cocoonbox-desk VERB [key=value ...|JSON]` sends one call and prints the result, as `desk` does on Linux.

## Modules

| File | Role |
|---|---|
| `desk.rs` | The verbs: window choice, find, outline, the press plan (select, open menus, click when a hit test finds the element, else its action), crops |
| `model.rs` | Neutral windows, nodes and trees, and the `Access` trait each OS backend implements |
| `uia.rs`, `uia_roles.rs` | Windows: top-level windows from the Win32 window list, one UIA cache request over the control view per tree; control types and Toggle/ExpandCollapse/SelectionItem/Value states mapped to ARIA roles and desk states; ids are runtime ids |
| `ax.rs`, `ax_roles.rs` | macOS: a walk with one batched AX read per element, at most 2500 nodes and 4 s; the app's menu bar joins its main window's tree; sheets are dialogs; bounds in pixels by the display scale |
| `worker.rs` | The backend's own thread; a call past its limit leaves that thread behind and the next call starts a fresh one |
| `driver.rs` | cua-driver client (line JSON over its socket or named pipe) for input, windows, launch and capture, all addressed to the desktop |
| `keys.rs` | The key vocabulary per OS (`cmd` on macOS, `win` on Windows) and the UIA and AX shortcut forms |
| `server.rs`, `rpc.rs` | The endpoint and the framing |

## Build

`cargo build --release` on macOS; `cargo build --release --target x86_64-pc-windows-gnu` (mingw-w64) for Windows. The unit tests drive fake backends and a fake driver; none reads an accessibility tree or sends input.
