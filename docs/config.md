# Configuration

The daemon reads this file when it starts. After an edit, quit it (`pkill -f 'cocoon-shuttle serve'`; Windows: end `cocoon-shuttle.exe` in Task Manager) and the next tool call other than `status` starts it with the new settings.

shuttle reads one TOML file:

| OS | Path |
|---|---|
| macOS, Linux | `$XDG_CONFIG_HOME/cocoon-shuttle/config.toml`, else `~/.config/cocoon-shuttle/config.toml` |
| Windows | `%APPDATA%\cocoon-shuttle\config.toml` |

## Models (bring your own key)

shuttle calls models through one OpenAI-compatible endpoint with one key. The default is [OpenRouter](https://openrouter.ai), so the quickstart is:

1. Create a key at <https://openrouter.ai/settings/keys>.
2. `export OPENROUTER_API_KEY=sk-or-...` in the environment the daemon starts from (on macOS, see [Keys](#keys)).
3. Done: no configuration file is needed. Any OpenRouter key reaches every default model; the `decide` model, `typesafe/jev-1.14`, answers on OpenRouter's Decisions API, so its public model list shows it under `?output_modalities=decisions` and not in the default chat list.

The desk runtime calls models in four roles, each a model name on that endpoint:

| Role | Default model | What it does | Used by |
|---|---|---|---|
| `decide` | `typesafe/jev-1.14` | Picks one element from candidates, or answers a yes/no question, with probabilities, over text | `gui_plan` window checks, `expect` steps, `find`, and the text pick when the screenshot pick fails; `desk.decide` in scripts |
| `verify` | the `decide` model | Answers the same over a screenshot, within 5 s | `gui_plan` when a step's target matches more than one element, and its check after a grounded click; `desk.look` in scripts |
| `ground` | `qwen/qwen3-vl-235b-a22b-instruct` | Returns the point of a described element in a screenshot, as `point_2d` from 0 to 1000 | `gui_plan` when the accessibility tree has no match; `desk.ground` |
| `ask` | `anthropic/claude-sonnet-5` | Answers a free-form question about screenshots | `gui_plan` `expect` steps the text check does not confirm, and toggle states the accessibility tree does not report; `desk.ask` |

Every field is optional; this file spells out the defaults:

```toml
base_url = "https://openrouter.ai/api/v1"                 # POST {base_url}/chat/completions
decisions_url = "https://openrouter.ai/api/alpha/decisions"
api_key_env = "OPENROUTER_API_KEY"
timeout_ms = 60000
decide_api = "decisions"                                 # decisions | chat
# decide_build = "typesafe/jev-1.14-########"           # only these builds may answer decide and verify

[models]
decide = "typesafe/jev-1.14"
# verify = "..."                                         # defaults to decide
ground = "qwen/qwen3-vl-235b-a22b-instruct"
ask = "anthropic/claude-sonnet-5"
```

- shuttle makes two calls, both non-streaming: `POST {base_url}/chat/completions` for `ask`, `ground`, `decide` and `verify` with `decide_api = "chat"`, and `POST {decisions_url}` with `{model, state, questions}` for `decide` and `verify` with `decide_api = "decisions"`. Chat bodies carry `max_tokens` (100 for `ground`, 400 for `ask`, 800 for `decide` over chat), which OpenRouter accepts for every model.
- **A local server** (vLLM, llama.cpp, Ollama's OpenAI API): set `base_url` to its `/v1` root, `decide_api = "chat"` and the model names it serves. A server without a key needs none: with no key shuttle sends no `Authorization` header.
- **Your own gateway**: set `base_url` to its OpenAI-compatible root and `decisions_url` to its decisions route, such as `base_url = "https://gw.example/v1"` and `decisions_url = "https://gw.example/v1/decisions"`.
- `decide_build` pins the builds a `decide` or `verify` answer may come from: `#` stands for one digit and every other character for itself, so `typesafe/jev-1.14-########` takes any dated jev-1.14 build and refuses an alias or another version. The build is the `model` the endpoint names in its reply; with the key set, an answer that names none, or another, fails the call with that build in the error. Unset, any build answers; an empty value fails the file.
- `timeout_ms` bounds each call; every screenshot judgment (`desk.look`, the `verify` role) caps it at 5 s. One call is one request: shuttle does not retry it.

### `decide` without a decisions endpoint

`gui_plan` was tuned against a decisions endpoint, which returns calibrated probabilities per answer. With `decide_api = "chat"`, shuttle asks the chat model for the same answers as JSON and reads the probabilities it writes. Those numbers are self-reported, not calibrated, and the plan executor's thresholds (a 0.7 bar for a pick, 0.5 for a `set` field, a 0.6 runner-up margin, 0.5 to 0.95 for a crop check) were set on calibrated ones. Expect more escalations and more wrong picks than with a decisions endpoint.

## Keys

- The key comes from `api_key` in the file when set, else from the variable `api_key_env` names. Prefer the variable: the file then holds no secret.
- On macOS the daemon starts from `Cocoon Shuttle.app` through LaunchServices and does not see your shell's environment: there, set the variable with `launchctl setenv OPENROUTER_API_KEY ...` (until logout) or put `api_key` in the file.
- On macOS and Linux, a file that holds an `api_key` must be readable by its owner only (mode 0600); shuttle refuses it otherwise, as ssh refuses a readable private key. On Windows, `%APPDATA%` is per user; shuttle does not check its ACL.
- Keys stay in the daemon: scripts call models through it over the desk wire, run without `OPENROUTER_API_KEY` and the configured `api_key_env` in their environment, and errors and logs print a key as `***`. A script runs with your user's rights, so it can read a key kept in this file; keep the key in the variable.

## Saved flows

`gui_plan` saves passing plans as flows in a `flows` directory next to `config.toml`, as one JSON file per flow, an `INDEX`, a run log `.runs.jsonl`, `.pending.json` (the steps of the last plans, which a later `save` stores) and a git history of the changes when git is installed (on macOS, when the Command Line Tools are: the flow history never runs the `git` stub that would open their installer; git that a `script_run` script runs is that script's own call). Flows hold the values plans typed, so the directory is readable by its owner only (0700) and its files are created 0600.

## Safety

```toml
[safety]
apps = []                             # app allowlist; empty allows every app
kill_hotkey = "ctrl+alt+cmd+escape"   # ctrl+alt+shift+escape off macOS; "off" registers none
indicator = true                      # menu-bar item; tray icon on Windows
confirm = "off"                       # "coordinate" or "every" asks first

[tools]
script_run = true                     # false turns script_run off

[driver]
overlay = true                        # false: no agent cursor; no effect on macOS
```

The safety controls live in this file, so while it fails to load every tool call except `status` answers with its error, desk calls included; only `cocoon-shuttle stop` and `resume` still work. `kill_hotkey` takes modifiers (`ctrl`, `alt`, `shift`, `cmd`, which is the Windows key on Windows) and one key (a letter, a digit, `f1`–`f12`, `escape`, `space`, `return` or `tab`), or is `off` for no global hotkey; on macOS and Windows a value that does not parse fails the file like any other error. [safety.md](safety.md) describes each control. They are guard rails for the `desk`, `screenshot` and `gui_plan` tools that hold only with `script_run = false`: a `script_run` script is your own user's code and can change or bypass them, as [safety.md](safety.md#what-these-controls-are) explains.

`[driver] overlay = false` starts the bundled cua-driver with `--no-overlay`, for a desktop nobody watches, such as a cloud box. It has no effect on macOS, since the driver's front-window tracking needs its overlay, and the daemon says so in its log.
