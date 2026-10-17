# shuttle-desk

The desk verbs on macOS and Windows: `desk_windows`, `desk_snapshot`, `desk_find`, `desk_read`, `desk_press`, `desk_act`, `desk_at`, `desk_key`, `desk_type`, `desk_click`, `desk_focus`, `desk_close`, `desk_launch`, `desk_shot` and `desk_screen`, answered over each OS's accessibility tree with input, windows, launch and capture through cua-driver. It speaks the toolhost wire of [docs/wire.md](../../docs/wire.md) and takes the wire, the desk call arguments and the window and name matching from [desk-core](../desk-core), so `runtime/lib/desk.mts` and `gui_plan.mts` run unchanged on it. `server::serve_native` serves this OS's desk at an endpoint; the `cocoon-shuttle` binary runs it (`cocoon-shuttle serve`) and calls it (`cocoon-shuttle desk VERB`).

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
| `server.rs`, `rpc.rs` | The endpoint, the framing and `serve_native`, which picks this OS's backend |

## Build

`cargo build` at the workspace root on macOS or Windows; `cargo clippy --target x86_64-pc-windows-gnu` checks the Windows code from another host. The unit tests drive fake backends and a fake driver; none reads an accessibility tree or sends input.
