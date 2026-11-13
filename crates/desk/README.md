# shuttle-desk

The desk verbs on macOS, Windows and Linux (X11): `desk_windows`, `desk_unanswered`, `desk_wait`, `desk_snapshot`, `desk_find`, `desk_read`, `desk_press`, `desk_act`, `desk_at`, `desk_key`, `desk_type`, `desk_click`, `desk_scroll`, `desk_drag`, `desk_still`, `desk_focus`, `desk_close`, `desk_launch`, `desk_shot`, `desk_screen` and `desk_web`, answered over each OS's accessibility tree, with input, window focus, launch and capture through cua-driver, and the computer verbs of a vision agent. It speaks the desk wire of [docs/wire.md](../../docs/wire.md) and takes the wire, the desk call arguments, the window and name matching and the screenshot crops from [desk-core](../desk-core), so `runtime/lib/desk.mts` and `gui_plan.mts` run unchanged on it. `server::native()` is this OS's desk and `server::serve` serves a handler at an endpoint; the `shuttle` daemon serves the desk behind its own handler (`shuttle serve`), and `shuttle desk VERB` calls it.

## Modules

| File | Role |
|---|---|
| `desk.rs` | The verbs: window choice, find, outline, waits, clicks at a point |
| `press.rs` | The press plan: select, open menus, click when a hit test finds the element, else its action |
| `still.rs` | The block model of `desk_still`: churning and toggling blocks stop counting |
| `allow.rs` | The app allowlist: which windows and elements a call may list, read, shoot and act on |
| `auto.rs` | Which background refusals `auto` input runs again in the foreground: those raised before any input was sent |
| `model.rs` | Neutral windows, nodes and trees, and the `Access` trait each OS backend implements |
| `uia.rs`, `uia_roles.rs` | Windows: top-level windows from the Win32 window list, one UIA cache request over the control view per tree, one per child when the window owns a dialog; control types and Toggle/ExpandCollapse/SelectionItem/Value states mapped to ARIA roles and desk states; ids are runtime ids |
| `ax.rs`, `ax_roles.rs` | macOS: a walk with one batched AX read per element, at most 2500 nodes and 4 s; the app's menu bar joins a window's tree for `find` and for a snapshot that asks for `menus`; sheets are dialogs; bounds in pixels by the display scale |
| `spi.rs`, `atspi.rs` | Linux: a window's tree from its app's AT-SPI cache, filled in by live D-Bus calls on the worker's own runtime; live reads and actions on one node; window-manager close |
| `screen.rs` | Window placements, rectangle clipping and the block hashes the settle compares, on every OS |
| `xshm.rs`, `noshm.rs` | Linux: the root window over MIT-SHM, its block hashes with the docks masked, the mapped windows, the desktop shell's windows and pids, and the modal dialogs the screen does not show; off Linux a grabber that never opens |
| `xtest.rs` | Linux: typing through XTest on the core keyboard for text the keyboard map holds |
| `scope.rs` | Linux: a launched app moves into a systemd scope of its own |
| `entry.rs` | Linux: the desktop entries a launch name can name |
| `computer.rs`, `volatile.rs` | The computer verbs, on cua-driver captures off Linux and MIT-SHM on Linux, and the Linux sampler of the screen blocks that change while no action runs, which their settle ignores |
| `worker.rs` | The backend's own thread; a call past its limit leaves that thread behind and starts a fresh one |
| `driver.rs` | cua-driver client (line JSON over its socket or named pipe) for input, window focus, launch and capture |
| `keys.rs` | The key vocabulary, one alias table for every OS (the logo key is `cmd` on macOS, `win` on Windows, `super` on Linux; a name no driver knows fails), the UIA and AX shortcut forms, and the unshifted US-layout key a Windows background type sends for a character |
| `pipesec.rs` | Windows: who may open the daemon's pipe, whether a pipe's client runs as this user, and whether a pipe belongs to the user its name carries or to Administrators |
| `server.rs` | The endpoint (socket or pipe, one daemon per endpoint), the `Handler` trait, `serve`, `call` and `native`, which picks this OS's backend |

## Build

`cargo build` at the workspace root; `cargo clippy --target x86_64-pc-windows-gnu` checks the Windows code from another host. The unit tests drive fake backends and a fake driver; none reads an accessibility tree or sends input.
