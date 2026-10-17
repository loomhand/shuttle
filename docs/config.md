# Configuration

On `main` (unreleased) the daemon reads the model settings below; the released v0.0.1 reads no configuration. The `[safety]` and `[tools]` tables land with the P1 safety work.

shuttle reads one TOML file:

| OS | Path |
|---|---|
| macOS, Linux | `$XDG_CONFIG_HOME/cocoon-shuttle/config.toml`, else `~/.config/cocoon-shuttle/config.toml` |
| Windows | `%APPDATA%\cocoon-shuttle\config.toml` |

## Models (bring your own key)

shuttle calls models through one OpenAI-compatible endpoint with one key. The default is [OpenRouter](https://openrouter.ai), so the quickstart is:

1. Create a key at <https://openrouter.ai/settings/keys>.
2. `export OPENROUTER_API_KEY=sk-or-...` in the environment the daemon starts from (on macOS, see [Keys](#keys)).
3. Done: no configuration file is needed.

The desk runtime calls models in four roles, each a model name on that endpoint:

| Role | Default model | What it does | Used by |
|---|---|---|---|
| `decide` | `typesafe/jev-1.14` | Picks one element from candidates, or answers a yes/no question, with probabilities, over text or a screenshot | `gui_plan` when a step's target matches more than one element; `desk.decide` and `desk.look` in scripts |
| `verify` | the `decide` model | Checks a crop around a grounded point, within 5 s | `gui_plan` after a grounded click |
| `ground` | `qwen/qwen3-vl-235b-a22b-instruct` | Returns the point of a described element in a screenshot, as `point_2d` from 0 to 1000 | `gui_plan` when the accessibility tree has no match; `desk.ground` |
| `ask` | `anthropic/claude-sonnet-5` | Answers a free-form question about screenshots | `desk.ask` |

Every field is optional; this file spells out the defaults:

```toml
base_url = "https://openrouter.ai/api/v1"                 # POST {base_url}/chat/completions
decisions_url = "https://openrouter.ai/api/alpha/decisions"
api_key_env = "OPENROUTER_API_KEY"
timeout_ms = 60000
decide_api = "decisions"                                 # decisions | chat

[models]
decide = "typesafe/jev-1.14"
# verify = "..."                                         # defaults to decide
ground = "qwen/qwen3-vl-235b-a22b-instruct"
ask = "anthropic/claude-sonnet-5"
```

- shuttle makes two calls, both non-streaming: `POST {base_url}/chat/completions` for `ask`, `ground`, and `decide` with `decide_api = "chat"`, and `POST {decisions_url}` with `{model, state, questions}` for `decide` and `verify` with `decide_api = "decisions"`. Chat bodies carry `max_tokens` (100 for `ground`, 400 for `ask`, 800 for `decide` over chat), which OpenRouter accepts for every model.
- **A local server** (vLLM, llama.cpp, Ollama's OpenAI API): set `base_url` to its `/v1` root, `decide_api = "chat"` and the model names it serves. A server without a key needs none: with no key shuttle sends no `Authorization` header.
- **Your own gateway**: set `base_url` to its OpenAI-compatible root and `decisions_url` to its decisions route, such as `base_url = "https://gw.example/v1"` and `decisions_url = "https://gw.example/v1/decisions"`.
- `timeout_ms` bounds each call; a crop check caps it at 5 s. One call is one request: shuttle does not retry it.

### `decide` without a decisions endpoint

`gui_plan` was tuned against a decisions endpoint, which returns calibrated probabilities per answer. With `decide_api = "chat"`, shuttle asks the chat model for the same answers as JSON and reads the probabilities it writes. Those numbers are self-reported, not calibrated, and the plan executor's thresholds (a 0.7 bar for a pick, a 0.6 runner-up margin, 0.5 for a crop check) were set on calibrated ones. Expect more escalations and more wrong picks than with a decisions endpoint.

## Keys

- The key comes from `api_key` in the file when set, else from the variable `api_key_env` names. Prefer the variable: the file then holds no secret.
- On macOS the daemon starts from `Cocoon Shuttle.app` through LaunchServices and does not see your shell's environment: there, set the variable with `launchctl setenv OPENROUTER_API_KEY ...` (until logout) or put `api_key` in the file.
- On macOS and Linux, a file that holds an `api_key` must be readable by its owner only (mode 0600); shuttle refuses it otherwise, as ssh refuses a readable private key. On Windows, `%APPDATA%` is per user; shuttle does not check its ACL.
- Keys stay in the daemon: scripts call models through it over the desk wire, run without `OPENROUTER_API_KEY` and the configured `api_key_env` in their environment, and errors and logs print a key as `***`. A script runs with your user's rights, so it can read a key kept in this file; keep the key in the variable.

## Saved flows

`gui_plan` saves passing plans as flows in a `flows` directory next to `config.toml`, as one JSON file per flow, an `INDEX`, a run log `.runs.jsonl` and a git history of the changes when git is installed.

## Safety (P1, not built yet)

Until the safety controls land, a file with these tables does not load, and every model call answers with that error; this is the planned shape:

```toml
[safety]
kill_hotkey = "ctrl+alt+cmd+escape"  # default; "ctrl+alt+shift+escape" on Windows and Linux
indicator = true                     # the menu-bar/tray item and the cursor overlay while shuttle acts
confirm = "off"                      # off | coordinate | every
apps = []                            # app allowlist; empty allows every app
user_idle_ms = 1500                  # how long you must be idle before a coordinate click

[tools]
script_run = true
```

[safety.md](safety.md) describes each control.
