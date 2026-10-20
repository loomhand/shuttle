# Desk wire

One wire connects `cocoon-shuttle mcp` to the daemon, the runtime (`desk.mts`) to the daemon, and the CLI (`cocoon-shuttle desk VERB`) to the daemon. It is implemented once in `desk_core::wire`.

## Endpoint

| OS | Default endpoint |
|---|---|
| macOS | `~/Library/Caches/cocoon-shuttle/desk.sock`, mode 0600 |
| Windows | `\\.\pipe\cocoon-shuttle-desk`, local clients only |
| Linux | `$XDG_CACHE_HOME/cocoon-shuttle/desk.sock`, else `~/.cache/cocoon-shuttle/desk.sock`, mode 0600 |

`[daemon] socket_group` in `config.toml` opens the macOS or Linux socket to that group: mode 0660, owned by the group ([config.md](config.md#daemon-endpoint)). On Windows the pipe admits SYSTEM and its user, and `socket_group` names one more account or SID that may read and write it. `SHUTTLE_SOCKET` overrides the endpoint for the binary, and the daemon hands its endpoint to the runtime, the plugins and the services in the same variable.

## Framing

- **One call per connection.** The client writes one frame and reads one frame, and keeps its side of the connection open until the reply: closing it, or shutting down its write side, cancels a call the daemon answers itself. A cancelled run is stopped as by `safety_stop` (its process group killed on macOS and Linux, the Node process on Windows) and its turn is free at once; a desk action the driver or the accessibility API has already started finishes on its own, as after a stop. A call of a [service plugin](#service-plugins)'s tool is the exception: after its first frame the connection belongs to the service, and a client that closes its side half-closes the connection toward the service instead of cancelling.
- **Frame:** a big-endian u32 length, then that many bytes of JSON. A frame over 64 MiB is refused.
- **Request:** `{"agent_id": string, "tool": string, "args": object}`; `agent_id` defaults to `""` and `args` to `null`.
- **Response:** `{"text": string, "is_error": bool, "image_b64"?: string, "secrets"?: [[field, value]]}`. `text` holds the result, JSON for the desk verbs; `image_b64` is a PNG when the call took one; `secrets` carries (field, value) pairs that a host embedding the wire must scrub from what it relays; shuttle never sends it. A failure is `is_error: true` with the reason in `text`, never a closed connection.

## Desk calls

Desk calls name `desk_<verb>` tools:

| Verb | Arguments | Result |
|---|---|---|
| `windows` | | the top-level windows: `id`, `title`, `role`, `pid`, `active`, and `app` (the app's name on macOS, the executable on Windows and Linux) when the OS says; on Linux a modal dialog the screen does not show also has `modal` (`hidden`, `off the screen` or `behind "TITLE"`) and `blocks` (the title of the window it blocks), and a window of a process that owns a dock or desktop window (`_NET_WM_WINDOW_TYPE`), such as the panel or the desktop, has `shell: true`. Optional fields are left out when unknown, so older readers of the wire are unaffected |
| `wait` | `since` (a `windows` result), `timeout_ms` | `changed` and `windows`: answers once the window list differs from `since`, or with the same list once `timeout_ms` has passed (5 s at most), counting the time spent reading lists, with a last read at the deadline. The desktop desk checks a cheap on-screen window fingerprint every 10 ms and reads the full list at once when it changes, and otherwise every 100 ms or the last read's duration, whichever is longer; the Android desk has no fingerprint and reads the full list every 10 ms or the last read's duration, whichever is longer, never sleeping past the deadline; `since` and the list are compared after the app allowlist filters both |
| `snapshot` | `window?`, `pid?`, `id?` (a window id from `windows`, in place of `window`), `keys?`, `menus?`, `bounds?`, `text?`, `max_chars?` (with `text`) | a window's tree as nodes (`ref`, `id`, `parent`, `depth`, `role`, `name`, `states`, ...), or an outline with `text`; a Linux node read with `keys` also carries its raw AT-SPI key binding as `keys`; on macOS the app's menu bar comes with a window only with `menus` (omitted or false: no app menu bar in the snapshot), and other backends ignore it |
| `find` | `name`, `role?`, `window?`, `pid?` | the window and the nodes whose name matches |
| `read` | `id`, `text?` | one node with its value and text |
| `press` | `id` | presses a node: select a tab or row; a menu entry runs its action; a hidden menu opens the menus above it and a menu is clicked at its centre; another node is clicked at its centre when a hit test there finds it, else runs its default action |
| `act` | `id`, `action` (`click`, `action`, `select`, `focus`) | one action on a node |
| `at` | `x`, `y`, `window?`, `pid?` | the node at a screen point and its ancestors |
| `key` | `keys` (`ctrl+s`, `cmd+shift+t`, ...) | sends a key combination |
| `type` | `text` | types text; on an Android device `typed` says whether the field shows it (null: not checked, for a password field, a web view, or text the field changed otherwise) |
| `click` | `x`, `y`, `button?`, `count?` | clicks a screen point |
| `scroll` | `direction` (`up`, `down`, `left`, `right`), `amount?` (notches or key repeats, default 3, the driver caps it at 50), `x?`, `y?` | with `x` and `y`, wheel notches at that screen point, which scroll whatever is under it; without them, scroll keys to the front app window, by its pid and window id: the driver's topmost window, or with an app allowlist the window the allowlist picks and checks when the driver lists it under the same title, else that app's topmost window. On Windows, whose apps drop posted scroll keys, and on Linux, where the front window is that of the focused app, it wheels notches at that window's center instead, and fails when another window covers that center. `x` without `y` is an error. The allowlist gates the window at the point, or that front window |
| `drag` | `x`, `y`, `to_x`, `to_y` | presses at (`x`, `y`), moves and releases at (`to_x`, `to_y`), in desk coordinates; the allowlist gates both ends |
| `still` | `timeout_ms?` (default 600, at most 5000), `hold_ms?` (default 40) | `still`, `changed`, `ms` and `volatile`: answers once no 8-pixel block of a 320-pixel capture has changed in a way that counts for `hold_ms`, or after `timeout_ms` with `still: false`; `changed` says whether one did during the wait. A change counts when a pixel moves by more than 24 in a channel and the block is not volatile: a block that changed through three or more contents during the wait (a clock, a spinner, a video) stops counting, and one that went back to a content it showed (a caret) counts only with new contents; `volatile` counts those blocks. `timeout_ms` is a polling deadline, checked between captures, so a slow capture can carry the answer past it. Each check costs one driver capture on macOS and Windows; it returns no pixels and watches the whole screen, whatever the allowlist |
| `focus`, `close` | `window`, `pid?` | focuses or closes a window |
| `launch` | `app` | starts an app; on Linux the result also carries `scoped`, false when the app stays in the daemon's cgroup |
| `shot` | `x?`, `y?`, `w?`, `h?`, `path?` (Linux) | `width` and `height`, with the PNG of the screen or a rectangle in `image_b64`; on Linux it is read over MIT-SHM, or from the driver's capture when its cursor overlay shows or MIT-SHM is out of reach, and with `path` the PNG also goes to that file and the text carries `path` |
| `screen` | | `width` and `height` in pixels |

## Computer calls

A vision agent drives the screen with these, by screenshot pixels and element indexes rather than desk ids. The Linux X11 desk answers them; on macOS and Windows each answers `VERB: not on this OS yet: the computer verbs run on Linux X11`, and an Android device answers its own `computer_state`, `computer_act`, `computer_end` and `look`. With an app allowlist they are refused, since they see and click the whole screen. `session` names the driver session whose elements and screenshot scale a call uses. Without it, a call uses the caller's own default session, named after its `agent_id`; calls with neither `agent_id` nor `session`, such as `cocoon-shuttle desk`, share one default session. A host ends a session with `desk_computer_end`. The driver refuses an ended name for good, so once a default session ends, every caller's next call without `session` opens its default under a new name, and its next element action needs a new `computer_state`.

| Verb | Arguments | Result |
|---|---|---|
| `computer_state` | `session?`, `pid?`, `window_id?`, `query?`, `max_edge?`, `workspace_only?` | the windows frontmost first (pid, window_id, app, title, geometry), a line per modal dialog the screen does not show, the elements `[index]` of the frontmost app window or of `pid` from the driver's window state, at most 300, with centres in screenshot pixels, and a screenshot of the whole screen scaled to a long edge of `max_edge` (default 1024, 0 for full size), WebP or JPEG, whose size and input tokens the text gives |
| `computer_act` | `session?`, `action` (`click`, `left_click`, `double_click`, `right_click`, `triple_click`, `type`, `key`, `hotkey`, `scroll`, `drag`, `wait`, `launch_app`), `element?` or `x?`, `y?` (screenshot pixels), `to_x?`, `to_y?`, `text?`, `key?`, `keys?`, `direction?`, `amount?`, `app?`, `button?`, `seconds?`, `foreground?`, `max_edge?`, `scale?`; or `actions` (a list of such actions), `max_edge?`, `scale?` | one action, or each of the list in order up to the first failure, waited out until the screen is still; the text names the windows the action opened, closed or brought to the front, and the screenshot follows. `scale` maps x,y by the screenshot the caller saw instead of the session's latest; a list maps every step by the screenshot before it. A point off the screen and a hidden button of a GTK app are refused. `typed N of M` counts the characters that landed |
| `computer_end` | `session` | ends the driver session and forgets its elements and scale |
| `computer_scale` | `session?` | `scale`: screen pixels per pixel of the session's latest screenshot, else of any session's |
| `look` | `session?`, `max_edge?`, `workspace_only?`, `region?` (`{x, y, w, h}` in screenshot pixels) | a screenshot of the whole screen, or of `region` at full resolution with the mapping of its pixels to `computer_act` x,y |
| `title_at` | `x`, `y` (screen pixels) | `title`: whether the point shows a window title, in a desktop panel or a window manager's title bar |

## Runs

`script_run` (`code` or an absolute `script` path, which the MCP server resolves against the agent's working directory; `params`, default `{}`; `timeout_s` up to 110, default 90) runs one module on the bundled Node, with the desk API as the global `desk`, and answers `{result, logs, stderr, exit_code?, duration_ms, error?: {kind, message}}` (no `exit_code` after a timeout or a stop): `kind` is `import` (the module or the desk API failed to load), `no_main` (the module exports no `main` function), `exception` (`main` threw; `message` is the stack), `timeout`, `stopped` (the user stopped shuttle) or `exit` (Node exited without saying why). A report that carries `error` answers with `is_error: true` and the whole report as its text, so an MCP client sees the run fail with its logs. `gui_plan` runs the runtime's plan executor with its arguments and answers the plan's result, or an error when the plan reports one. Both run with `SHUTTLE_SOCKET` naming the daemon's endpoint, `SHUTTLE_AGENT` naming the run (the runtime sends it as `agent_id`, so the run's own desk actions pass while it owns the desk) and without the model key variables (`OPENROUTER_API_KEY` and the configured `api_key_env`); logs and stderr keep their first 32 KiB. While `config.toml` fails to load, every call except `safety_stop` and `safety_resume` answers with its error, since the key variable and the safety controls are in it. `safety_stop` and `safety_resume` (no arguments, what `cocoon-shuttle stop` and `resume` send) halt every call, ending the run and the desk and model calls in flight, and lift the halt; no MCP tool and no runtime call sends them, and on macOS and Linux the daemon answers them only to a peer that runs as its own user.

## Plugins

A plugin adds one tool that runs scripts with `script_run`'s contract and its own program in place of Node. This section is the schema's source of truth.

- **Where.** One `*.json` file per tool in the plugin directory: `tools.d` next to `config.toml`, or the directory `[tools] plugins` names ([config.md](config.md)). The daemon reads it when it starts and the MCP server when an agent starts it, in file-name order.
- **Spec.** `{"name": string, "description": string, "bin": string, "resources"?: [{"uri": string, "path": string, "mime": string, "description": string}]}`, and no other field.
  - `name` matches `^[a-z][a-z0-9_]*_run$` and is no built-in tool (`gui_plan`, `script_run`, `desk`, `screenshot`, `status`) and no earlier file's name.
  - `description` is at most 2048 characters.
  - `bin` and each resource `path` are absolute.
  - A file that breaks any rule, does not parse or holds an unknown field is skipped whole, with one log line naming the file and the reason; no part of it loads.
- **Calls.** The tool takes `script_run`'s arguments and answers its report. The daemon runs `bin` exactly as it runs Node for `script_run`: the runtime's entry module as the one argument, the same `SHUTTLE_SOCKET`, `SHUTTLE_SCRIPT`, `SHUTTLE_RUN`, `SHUTTLE_FLOWS` and `SHUTTLE_AGENT`, the result and error files in `SHUTTLE_RUN`, the same timeout, stop and output caps, and no model key variables. A desk call the program makes on `SHUTTLE_SOCKET` goes through the same app allowlist, confirm mode and stop as every other desk call.
- **Switch.** `script_run = false` turns the plugins off with it: the MCP server lists none, and a call answers as `script_run` does.
- **MCP.** `tools/list` lists each plugin after the built-in tools, with `script_run`'s input schema. The resources of every plugin are listed by `resources/list` (`uri`, the file name as `name`, `mime` as `mimeType`, `description`) and served as text by `resources/read`; `initialize` names the `resources` capability when there is one.

A neutral example, `tools.d/demo.json`:

```json
{"name": "demo_run", "description": "Run a demo script.", "bin": "/usr/local/bin/demo-runner", "resources": [{"uri": "demo://GUIDE.md", "path": "/usr/local/share/demo/GUIDE.md", "mime": "text/markdown", "description": "The demo guide"}]}
```

## Service plugins

A service plugin is a long-lived program that serves its own tools on the desk wire. The daemon runs it as its child and hands it the connection of every call of one of its tools. This section is the schema's source of truth.

- **Where.** A `*.json` file in the plugin directory with a `service` key is a service; any other file is a plugin of [Plugins](#plugins). The same file-name order applies, and the daemon and the MCP server read the directory as for plugins.
- **Spec.** `{"service": string, "bin": string, "args"?: [string], "tools": [{"name": string, "description": string, "input_schema": object, "mcp"?: bool}]}`, and no other field, in the file or in a tool.
  - `service` matches `^[a-z][a-z0-9_]*$`, is at most 32 characters and is no earlier file's service.
  - `bin` is absolute; `args` defaults to none.
  - `tools` holds at least one tool. Each `name` matches `^[a-z][a-z0-9_]*$` and is at most 64 characters. It is no built-in tool (`gui_plan`, `script_run`, `desk`, `screenshot`, `status`), starts with none of `desk_`, `model_` and `safety_`, does not end in `_run`, which plugins take, and is no other tool of this file or an earlier service.
  - `description` is at most 2048 characters; `input_schema` is a JSON Schema whose `type` is `object`.
  - `mcp`, default `true`, lists the tool on MCP; with `false` the tool is reached on the desk wire only, which suits a tool that streams frames.
  - A file that breaks any rule is skipped whole, with one log line.
- **Process.** The daemon starts `bin` with `args` when it starts, in your home directory, in a process group of its own (on Windows, a Job object of its own), with:
  - `SHUTTLE_SERVICE_SOCKET`: on macOS and Linux the unix socket to listen on, `services/<service>.sock` beside the daemon's endpoint, in a directory of mode 0700 that only your user reaches, where the daemon removes a file left at that path before each start; on Windows the named pipe to serve, the daemon's pipe name followed by `-svc-<service>` (`\\.\pipe\cocoon-shuttle-desk-svc-files`), whose security must admit your user;
  - `SHUTTLE_SOCKET`: the daemon's endpoint, for the service's own desk calls, which pass the same app allowlist, confirm and stop as any other;
  - no model key variables (`OPENROUTER_API_KEY` and the configured `api_key_env`).
  When the service exits, the daemon kills the rest of its process group (on Linux, all but the apps the desk moved into scopes of their own) and starts it again after 1 s, doubling the wait on each exit up to 30 s; a service that ran 30 s or more starts again after 1 s. When the daemon exits, it sends the service SIGTERM, waits 3 s, then kills its group. A daemon that is killed outright cannot do this, so run it under a supervisor that kills its process group or cgroup on restart, as systemd does; otherwise a killed daemon leaves its services running. On Windows a service's exit, a stop of the daemon and the daemon's own end by any means, `taskkill /F` and logoff included, kill the service's Job object with every process in it: Windows has no SIGTERM, so a service gets no notice.
- **Calls.** The daemon reads a call's first frame. When its `tool` names a service tool, it connects to that service's socket, waiting up to 10 s while the service starts, on Windows checks that the process serving the pipe is in the service's Job object and otherwise answers an error naming that process, since any process can create a free pipe name, writes the frame exactly as it read it, and then copies bytes both ways. Nothing after the first frame is parsed, and no frame size or deadline applies. When either side closes its end, the daemon closes the other side's input and keeps passing on what the other side sends; the call ends once both sides have closed their ends, or at the first error. A call no service listens for within 10 s, or that the daemon cannot write, answers with an error frame.
  - A service call takes no desk turn: it runs alongside runs and desk actions.
  - Work a service starts for a call, such as a shell command, is the service's to end when its connection closes; shuttle does not end it.
- **Stop.** `safety_stop` refuses new service calls with `stopped by the user` and closes every connection in flight, without a reply, since its client may be inside a stream of its own frames. The service keeps running, and `safety_resume` lets calls through again.
- **Switch.** Installing a service file is what turns the service on: `script_run = false` turns plugins off but leaves services running, listed and reached.
- **MCP.** `tools/list` lists each service tool with `mcp` true after the plugins, with its own `input_schema`. `tools/call` sends one frame and reads one response frame through the daemon, so a tool the MCP server lists answers one response frame.
- **Cost.** A desk call pays nothing when no service is installed. With one installed, every call that is not spliced pays a read of its `tool` before its full decode, 104 to 150 ns on a 74-byte desk frame (Apple M2 Max), and one lookup in the service tool table. A service call pays one unix-socket connect and the copy through a 64 KiB buffer each way; a microbench (`cargo test -p shuttle --release splice_cost -- --ignored --nocapture`) measures it.

A neutral example, `tools.d/notes.json`:

```json
{"service": "notes", "bin": "/usr/local/bin/notes-service", "args": ["--serve"], "tools": [{"name": "notes_read", "description": "Read a note.", "input_schema": {"type": "object", "properties": {"title": {"type": "string"}}, "required": ["title"]}}, {"name": "notes_watch", "description": "Stream note changes.", "input_schema": {"type": "object"}, "mcp": false}]}
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
