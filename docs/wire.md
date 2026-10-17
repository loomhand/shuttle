# Desk wire

One wire connects `shuttle mcp` to the daemon, the runtime (`desk.mts`) to the daemon, and the CLI (`shuttle desk VERB`) to the daemon. It is the toolhost wire of cocoonbox, implemented once in `desk_core::wire`, so the runtime runs unchanged in both.

## Endpoint

| OS | Default endpoint |
|---|---|
| macOS | `~/Library/Caches/shuttle/desk.sock`, mode 0600 |
| Linux | `$XDG_RUNTIME_DIR/shuttle/desk.sock`, else `~/.cache/shuttle/desk.sock`, mode 0600 |
| Windows | `\\.\pipe\shuttle-desk`, local clients only |

`SHUTTLE_SOCKET` overrides the endpoint for the binary. The daemon hands its endpoint to the runtime in `TOOLHOST_SOCKET`.

## Framing

- **One call per connection.** The client writes one frame and reads one frame; closing the connection cancels the call.
- **Frame:** a big-endian u32 length, then that many bytes of JSON. A frame over 64 MiB is refused.
- **Request:** `{"agent_id": string, "tool": string, "args": object}`; `agent_id` defaults to `""` and `args` to `null`.
- **Response:** `{"text": string, "is_error": bool, "image_b64"?: string}`. `text` holds the result, JSON for the desk verbs; `image_b64` is a PNG when the call took one. A failure is `is_error: true` with the reason in `text`, never a closed connection.

## Desk calls

Desk calls name `desk_<verb>` tools:

| Verb | Arguments | Result |
|---|---|---|
| `windows` | | the top-level windows: `id`, `title`, `role`, `pid`, `active`, and `modal`/`blocks` for dialogs |
| `snapshot` | `window?`, `pid?`, `id?`, `keys?`, `bounds?`, `text?` | a window's tree as nodes (`ref`, `id`, `parent`, `depth`, `role`, `name`, `states`, ...), or an outline with `text` |
| `find` | `name`, `role?`, `window?` | the window and the nodes whose name matches |
| `read` | `id`, `text?` | one node with its value and text |
| `press` | `id` | presses a node: select, open menus, click on a hit, else its default action |
| `act` | `id`, `action` (`click`, `action`, `select`, `focus`) | one action on a node |
| `at` | `x`, `y`, `window?` | the node at a screen point and its ancestors |
| `key` | `keys` (`ctrl+s`, `cmd+shift+t`, ...) | sends a key combination |
| `type` | `text` | types text |
| `click` | `x`, `y`, `button?`, `count?` | clicks a screen point |
| `focus`, `close` | `window`, `pid?` | focuses or closes a window |
| `launch` | `app` | starts an app |
| `shot` | `x?`, `y?`, `w?`, `h?` | `png_b64` of the screen or a rectangle |
| `screen` | | `width` and `height` in pixels |

## Model calls (from P1)

The daemon answers `model_decide`, `model_ask` and `model_ground` with the providers of [config.md](config.md), so a script calls a model without holding a key. Their arguments and results follow the `decide`, `look`, `ask` and `ground` functions of `desk.mts`.
