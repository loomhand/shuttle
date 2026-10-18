# Desk wire

One wire connects `cocoon-shuttle mcp` to the daemon, the runtime (`desk.mts`) to the daemon, and the CLI (`cocoon-shuttle desk VERB`) to the daemon. It is the toolhost wire of cocoonbox, implemented once in `desk_core::wire`, so the runtime runs unchanged in both.

## Endpoint

| OS | Default endpoint |
|---|---|
| macOS | `~/Library/Caches/cocoon-shuttle/desk.sock`, mode 0600 |
| Windows | `\\.\pipe\cocoon-shuttle-desk`, local clients only |
| Linux | `~/Library/Caches/cocoon-shuttle/desk.sock` (the macOS path, until the P3 backend picks a Linux one), mode 0600 |

`SHUTTLE_SOCKET` overrides the endpoint for the binary. The daemon hands its endpoint to the runtime in `TOOLHOST_SOCKET`.

## Framing

- **One call per connection.** The client writes one frame and reads one frame. Closing the connection does not cancel a call the daemon has read: it runs to its end, and its reply is dropped.
- **Frame:** a big-endian u32 length, then that many bytes of JSON. A frame over 64 MiB is refused.
- **Request:** `{"agent_id": string, "tool": string, "args": object}`; `agent_id` defaults to `""` and `args` to `null`.
- **Response:** `{"text": string, "is_error": bool, "image_b64"?: string, "secrets"?: [[field, value]]}`. `text` holds the result, JSON for the desk verbs; `image_b64` is a PNG when the call took one; `secrets` carries card fields a host typed from a file (cocoonbox), and shuttle never sends it. A failure is `is_error: true` with the reason in `text`, never a closed connection.

## Desk calls

Desk calls name `desk_<verb>` tools:

| Verb | Arguments | Result |
|---|---|---|
| `windows` | | the top-level windows: `id`, `title`, `role`, `pid`, `active`, and `app` (the app's name on macOS, the executable on Windows and Linux) when the OS says; on Linux a modal dialog the screen does not show also has `modal` (`hidden`, `off the screen` or `behind "TITLE"`) and `blocks` (the title of the window it blocks). Optional fields are left out when unknown, so older readers of the wire are unaffected |
| `wait` | `since` (a `windows` result), `timeout_ms` | `changed` and `windows`: answers once the window list differs from `since`, or with the same list once `timeout_ms` has passed (5 s at most), counting the time spent reading lists, with a last read at the deadline. The desktop desk checks a cheap on-screen window fingerprint every 10 ms and reads the full list at once when it changes, and otherwise every 100 ms or the last read's duration, whichever is longer; the Android desk has no fingerprint and reads the full list every 10 ms or the last read's duration, whichever is longer, never sleeping past the deadline; `since` and the list are compared after the app allowlist filters both. The runtime polls `windows` on a toolhost that answers `unknown desk tool` |
| `snapshot` | `window?`, `pid?`, `id?` (a window id from `windows`, in place of `window`), `keys?`, `menus?`, `bounds?`, `text?`, `max_chars?` (with `text`) | a window's tree as nodes (`ref`, `id`, `parent`, `depth`, `role`, `name`, `states`, ...), or an outline with `text`; a Linux node read with `keys` also carries its raw AT-SPI key binding as `keys`; on macOS the app's menu bar comes with a window only with `menus` (omitted or false: no app menu bar in the snapshot), and other backends ignore it |
| `find` | `name`, `role?`, `window?`, `pid?` | the window and the nodes whose name matches |
| `read` | `id`, `text?` | one node with its value and text |
| `press` | `id` | presses a node: select a tab or row; a menu entry runs its action; a hidden menu opens the menus above it and a menu is clicked at its centre; another node is clicked at its centre when a hit test there finds it, else runs its default action |
| `act` | `id`, `action` (`click`, `action`, `select`, `focus`) | one action on a node |
| `at` | `x`, `y`, `window?`, `pid?` | the node at a screen point and its ancestors |
| `key` | `keys` (`ctrl+s`, `cmd+shift+t`, ...) | sends a key combination |
| `type` | `text` | types text |
| `click` | `x`, `y`, `button?`, `count?` | clicks a screen point |
| `scroll` | `direction` (`up`, `down`, `left`, `right`), `amount?` (notches or key repeats, default 3, the driver caps it at 50), `x?`, `y?` | with `x` and `y`, wheel notches at that screen point, which scroll whatever is under it; without them, scroll keys to the front app window, by its pid and window id: the driver's topmost window, or with an app allowlist the window the allowlist picks and checks when the driver lists it under the same title, else that app's topmost window. On Windows, whose apps drop posted scroll keys, it wheels notches at that window's center instead, and fails when another window covers that center. `x` without `y` is an error. The allowlist gates the window at the point, or that front window |
| `drag` | `x`, `y`, `to_x`, `to_y` | presses at (`x`, `y`), moves and releases at (`to_x`, `to_y`), in desk coordinates; the allowlist gates both ends |
| `still` | `timeout_ms?` (default 600, at most 5000), `hold_ms?` (default 40) | `still`, `changed`, `ms` and `volatile`: answers once no 8-pixel block of a 320-pixel capture has changed in a way that counts for `hold_ms`, or after `timeout_ms` with `still: false`; `changed` says whether one did during the wait. A change counts when a pixel moves by more than 24 in a channel and the block is not volatile: a block that changed through three or more contents during the wait (a clock, a spinner, a video) stops counting, and one that went back to a content it showed (a caret) counts only with new contents; `volatile` counts those blocks. `timeout_ms` is a polling deadline, checked between captures, so a slow capture can carry the answer past it. Each check costs one driver capture on macOS and Windows; it returns no pixels and watches the whole screen, whatever the allowlist |
| `focus`, `close` | `window`, `pid?` | focuses or closes a window |
| `launch` | `app` | starts an app |
| `shot` | `x?`, `y?`, `w?`, `h?`, `path?` (Linux) | `width` and `height`, with the PNG of the screen or a rectangle in `image_b64`; on Linux, read over MIT-SHM unless the driver's cursor overlay shows or MIT-SHM is out of reach, the text also carries `png_b64`, or with `path` the PNG goes to that file and the text carries `path` |
| `screen` | | `width` and `height` in pixels |

## Runs

`script_run` (`code` or an absolute `script` path, which the MCP server resolves against the agent's working directory; `params`, default `{}`; `timeout_s` up to 110, default 90) runs one module on the bundled Node, with the desk API as the global `desk`, and answers `{result, logs, stderr, exit_code?, duration_ms, error?: {kind, message}}` (no `exit_code` after a timeout or a stop): `kind` is `import` (the module or the desk API failed to load), `no_main` (the module exports no `main` function), `exception` (`main` threw; `message` is the stack), `timeout`, `stopped` (the user stopped shuttle) or `exit` (Node exited without saying why). `gui_plan` runs the runtime's plan executor with its arguments and answers the plan's result. Both run with `TOOLHOST_SOCKET` naming the daemon's endpoint, `SHUTTLE_AGENT` naming the run (the runtime sends it as `agent_id`, so the run's own desk actions pass while it owns the desk) and without the model key variables (`OPENROUTER_API_KEY` and the configured `api_key_env`); logs and stderr keep their first 32 KiB. While `config.toml` fails to load, every call except `safety_stop` and `safety_resume` answers with its error, since the key variable and the safety controls are in it. `safety_stop` and `safety_resume` (no arguments, what `cocoon-shuttle stop` and `resume` send) halt every call, ending the run and the desk and model calls in flight, and lift the halt; no MCP tool and no runtime call sends them.

## Plugins

A plugin adds one tool that runs scripts with `script_run`'s contract and its own program in place of Node. This section is the schema's source of truth; cocoonbox's `/etc/cocoonbox/tools.d` files use the same one.

- **Where.** One `*.json` file per tool in the plugin directory: `tools.d` next to `config.toml`, or the directory `[tools] plugins` names ([config.md](config.md)). The daemon reads it when it starts and the MCP server when an agent starts it, in file-name order.
- **Spec.** `{"name": string, "description": string, "bin": string, "resources"?: [{"uri": string, "path": string, "mime": string, "description": string}]}`, and no other field.
  - `name` matches `^[a-z][a-z0-9_]*_run$` and is no built-in tool (`gui_plan`, `script_run`, `desk`, `screenshot`, `status`) and no earlier file's name.
  - `description` is at most 2048 characters.
  - `bin` and each resource `path` are absolute.
  - A file that breaks any rule, does not parse or holds an unknown field is skipped whole, with one log line naming the file and the reason; no part of it loads.
- **Calls.** The tool takes `script_run`'s arguments and answers its report. The daemon runs `bin` exactly as it runs Node for `script_run`: the runtime's entry module as the one argument, the same `TOOLHOST_SOCKET`, `SHUTTLE_SCRIPT`, `SHUTTLE_RUN`, `SHUTTLE_FLOWS` and `SHUTTLE_AGENT`, the result and error files in `SHUTTLE_RUN`, the same timeout, stop and output caps, and no model key variables. A desk call the program makes on `TOOLHOST_SOCKET` goes through the same app allowlist, confirm mode and stop as every other desk call.
- **Switch.** `script_run = false` turns the plugins off with it: the MCP server lists none, and a call answers as `script_run` does.
- **MCP.** `tools/list` lists each plugin after the built-in tools, with `script_run`'s input schema. The resources of every plugin are listed by `resources/list` (`uri`, the file name as `name`, `mime` as `mimeType`, `description`) and served as text by `resources/read`; `initialize` names the `resources` capability when there is one.

A neutral example, `tools.d/demo.json`:

```json
{"name": "demo_run", "description": "Run a demo script.", "bin": "/usr/local/bin/demo-runner", "resources": [{"uri": "demo://GUIDE.md", "path": "/usr/local/share/demo/GUIDE.md", "mime": "text/markdown", "description": "The demo guide"}]}
```

## Model calls

The daemon answers these with the models of [config.md](config.md) (`crates/models`), so a script calls a model without holding a key. Each reply carries `usage`: `{model, input_tokens, output_tokens, cached_tokens}`.

| Tool | Arguments | Result |
|---|---|---|
| `model_decide` | `state` (text, JSON, or a list of text and `{type: "image_url", image_url: {url: "data:image/png;base64,..."}}` parts), `questions` (`{key: {type: "noul" \| "choice", instructions, criteria?}}`), `image` (the state holds a screenshot: the `verify` role answers, else `decide`), `timeout_ms` (caps the configured timeout; `desk.look` passes 5000) | `answers` (`{key: {probability}}` for a yes question, `{key: {choice, confidence, probabilities}}` for a choice), `served` (`{model, build, id}`: the model name asked, and the build and generation id that answered, null when the endpoint names none) |
| `model_ask` | `prompt`, `images` (PNGs as base64) | `text` |
| `model_ground` | `png_b64`, `target`, `area` (`{left?, top?, width, height}`) | `x`, `y`; a point outside 0 to 1000 is an error, and 1000 maps onto the last pixel of `area` |

### Coordinates

Desk coordinates are the space `desk_screen` reports and `desk_click`, `desk_at`, `desk_shot` and element `bounds` use: screen pixels on Windows (the desk host is per-monitor DPI aware) and on Linux, and on macOS the display's pixels (AX points times the backing scale). A screenshot need not have that size: `desk_shot` may return a downscaled image, and a Retina capture can be 2x.

`model_ground` takes `area`, the desk rectangle the PNG shows, and returns `x`, `y` in desk coordinates: the model answers a point from 0 to 1000 across and down the image, and the daemon maps it onto `area`, whatever the PNG's own size. Pass the rectangle the PNG was cropped from, never the PNG's width and height.
