# Desk wire

One wire connects `cocoon-shuttle mcp` to the daemon, the runtime (`desk.mts`) to the daemon, and the CLI (`cocoon-shuttle desk VERB`) to the daemon. It is the toolhost wire of cocoonbox, implemented once in `desk_core::wire`, so the runtime runs unchanged in both.

## Endpoint

| OS | Default endpoint |
|---|---|
| macOS | `~/Library/Caches/cocoon-shuttle/desk.sock`, mode 0600 |
| Windows | `\\.\pipe\cocoon-shuttle-desk`, local clients only |

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
| `shot` | `x?`, `y?`, `w?`, `h?` | `width` and `height`, with the PNG of the screen or a rectangle in `image_b64` |
| `screen` | | `width` and `height` in pixels |

## Runs

`script_run` (`code` or an absolute `script` path, which the MCP server resolves against the agent's working directory; `params`, default `{}`; `timeout_s` up to 110, default 90) runs one module on the bundled Node, with the desk API as the global `desk`, and answers `{result, logs, stderr, exit_code, duration_ms, error?: {kind, message}}`: `kind` is `import` (the module or the desk API failed to load), `no_main` (the module exports no `main` function), `exception` (`main` threw; `message` is the stack), `timeout` or `exit` (Node exited without saying why). `gui_plan` runs the runtime's plan executor with its arguments and answers the plan's result. Both run with `TOOLHOST_SOCKET` naming the daemon's endpoint, `SHUTTLE_AGENT` naming the run (the runtime sends it as `agent_id`, so the run's own desk actions pass while it owns the desk) and without the model key variables (`OPENROUTER_API_KEY` and the configured `api_key_env`); logs and stderr keep their first 32 KiB.

## Model calls

The daemon answers these with the providers of [config.md](config.md) (`crates/models`; the daemon routes them from P1), so a script calls a model without holding a key. Each reply carries `usage`: `{model, input_tokens, output_tokens, cached_tokens}`.

| Tool | Arguments | Result |
|---|---|---|
| `model_decide` | `state` (text, JSON, or a list of text and `{type: "image_url", image_url: {url: "data:image/png;base64,..."}}` parts), `questions` (`{key: {type: "noul" \| "choice", instructions, criteria?}}`), `image` (the state holds a screenshot: the `verify` role answers, else `decide`), `timeout_ms` (caps the configured timeout; the crop check passes 5000) | `answers` (`{key: {probability}}` for a yes question, `{key: {choice, confidence, probabilities}}` for a choice), `served` (`{model, build, id}`: the model name asked, and the build and generation id that answered, null when the endpoint names none) |
| `model_ask` | `prompt`, `images` (PNGs as base64) | `text` |
| `model_ground` | `png_b64`, `target`, `area` (`{left?, top?, width, height}`) | `x`, `y`; a point outside 0 to 1000 is an error, and 1000 maps onto the last pixel of `area` |

### Coordinates

Desk coordinates are the space `desk_screen` reports and `desk_click`, `desk_at`, `desk_shot` and element `bounds` use: screen pixels on Windows (the desk host is per-monitor DPI aware) and on Linux, and on macOS the display's pixels (AX points times the backing scale). A screenshot need not have that size: `desk_shot` may return a downscaled image, and a Retina capture can be 2x.

`model_ground` takes `area`, the desk rectangle the PNG shows, and returns `x`, `y` in desk coordinates: the model answers a point from 0 to 1000 across and down the image, and the daemon maps it onto `area`, whatever the PNG's own size. Pass the rectangle the PNG was cropped from, never the PNG's width and height.
