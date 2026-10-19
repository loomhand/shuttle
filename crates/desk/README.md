# shuttle-desk

The desk verbs on macOS, Windows and Linux (X11): `desk_windows`, `desk_wait`, `desk_snapshot`, `desk_find`, `desk_read`, `desk_press`, `desk_act`, `desk_at`, `desk_key`, `desk_type`, `desk_click`, `desk_scroll`, `desk_drag`, `desk_still`, `desk_focus`, `desk_close`, `desk_launch`, `desk_shot` and `desk_screen`, answered over each OS's accessibility tree, with input, window focus, launch and capture through cua-driver, and on Linux the computer verbs of a vision agent. It speaks the desk wire of [docs/wire.md](../../docs/wire.md) and takes the wire, the desk call arguments and the window and name matching from [desk-core](../desk-core), so `runtime/lib/desk.mts` and `gui_plan.mts` run unchanged on it. `server::native()` is this OS's desk and `server::serve` serves a handler at an endpoint; the `cocoon-shuttle` daemon serves the desk behind its own handler (`cocoon-shuttle serve`), and `cocoon-shuttle desk VERB` calls it.

## Modules

| File | Role |
|---|---|
| `desk.rs` | The verbs: window choice, find, outline, waits, clicks at a point |
| `press.rs` | The press plan: select, open menus, click when a hit test finds the element, else its action |
| `shot.rs` | Screenshot crops, with apps outside the allowlist painted over |
| `still.rs` | The block model of `desk_still`: churning and toggling blocks stop counting |
| `allow.rs` | The app allowlist: which windows and elements a call may list, read, shoot and act on |
| `model.rs` | Neutral windows, nodes and trees, and the `Access` trait each OS backend implements |
| `uia.rs`, `uia_roles.rs` | Windows: top-level windows from the Win32 window list, one UIA cache request over the control view per tree, one per child when the window owns a dialog; control types and Toggle/ExpandCollapse/SelectionItem/Value states mapped to ARIA roles and desk states; ids are runtime ids |
| `ax.rs`, `ax_roles.rs` | macOS: a walk with one batched AX read per element, at most 2500 nodes and 4 s; the app's menu bar joins its main window's tree; sheets are dialogs; bounds in pixels by the display scale |
| `spi.rs`, `atspi.rs` | Linux: a window's tree from its app's AT-SPI cache, filled in by live D-Bus calls on the worker's own runtime; live reads and actions on one node; window-manager close |
| `xshm.rs` | Linux: the root window over MIT-SHM, its block hashes with the docks masked, the mapped windows, the desktop shell's windows and pids, and the modal dialogs the screen does not show |
| `xtest.rs` | Linux: typing through XTest on the core keyboard for text the keyboard map holds |
| `scope.rs` | Linux: a launched app moves into a systemd scope of its own |
| `computer.rs`, `volatile.rs` | Linux: the computer verbs, and the sampler of the screen blocks that change while no action runs, which their settle ignores |
| `worker.rs` | The backend's own thread; a call past its limit leaves that thread behind and the next call starts a fresh one |
| `driver.rs` | cua-driver client (line JSON over its socket or named pipe) for input, window focus, launch and capture |
| `keys.rs` | The key vocabulary per OS (`cmd` on macOS, `win` on Windows, keysym aliases on Linux), the UIA and AX shortcut forms, and the keystroke of each typed character on Windows |
| `server.rs` | The endpoint (socket or pipe, one daemon per endpoint), the `Handler` trait, `serve`, `call` and `native`, which picks this OS's backend |

## Build

`cargo build` at the workspace root; `cargo clippy --target x86_64-pc-windows-gnu` checks the Windows code from another host. The unit tests drive fake backends and a fake driver; none reads an accessibility tree or sends input.
