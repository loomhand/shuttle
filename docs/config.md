# Configuration

> The configuration lands with P1. This page is the contract the code follows; v0.0.1 reads no configuration.

shuttle reads one TOML file:

| OS | Path |
|---|---|
| macOS, Linux | `$XDG_CONFIG_HOME/cocoon-shuttle/config.toml`, else `~/.config/cocoon-shuttle/config.toml` |
| Windows | `%APPDATA%\cocoon-shuttle\config.toml` |

## Models (bring your own key)

The desk runtime calls models in four roles. Each role names one endpoint; any OpenAI-compatible or Anthropic-compatible endpoint works, as does a decisions endpoint for the `decide` and `verify` roles.

| Role | What it does | Used by |
|---|---|---|
| `decide` | Picks one element from candidates, or answers a yes/no question, with probabilities, over text or a screenshot | `gui_plan` when a step's target matches more than one element; `desk.decide` and `desk.look` in scripts |
| `verify` | Checks a crop around a grounded point, within 5 s | `gui_plan` after a grounded click; defaults to `decide` |
| `ground` | Returns the point of a described element in a screenshot, as `point_2d` from 0 to 1000 | `gui_plan` when the accessibility tree has no match; `desk.ground` |
| `ask` | Answers a free-form question about screenshots | `desk.ask` |

```toml
[models.decide]
api = "openai"                      # openai | anthropic | decisions
base_url = "https://api.openai.com/v1"
model = "<a vision chat model>"
api_key_env = "OPENAI_API_KEY"      # or api_key = "..." in this file (mode 0600 only)
timeout_ms = 60000

[models.ground]
api = "openai"
base_url = "https://openrouter.ai/api/v1"
model = "<a grounding model that answers point_2d, such as a Qwen3-VL model>"
api_key_env = "OPENROUTER_API_KEY"

[models.ask]
api = "anthropic"
base_url = "https://api.anthropic.com"
model = "<a vision chat model>"
api_key_env = "ANTHROPIC_API_KEY"
```

- `api = "openai"` calls `POST {base_url}/chat/completions`; `api = "anthropic"` calls `POST {base_url}/v1/messages`; `api = "decisions"` calls `POST {base_url}/decisions`. Each kind is spoken natively; shuttle never translates one protocol into another.
- A role without an entry is off: a step that needs it escalates with `no model for <role>` instead of guessing.

### `decide` without a decisions endpoint

`gui_plan` was tuned against a decisions endpoint, which returns calibrated probabilities per answer. With `api = "openai"` or `"anthropic"`, shuttle asks the chat model for the same JSON and reads the probabilities it writes. Those numbers are self-reported, not calibrated, and the plan executor's thresholds (a 0.7 bar for a pick, a 0.6 runner-up margin, 0.5 for a crop check) were set on calibrated ones. Expect more escalations and more wrong picks than with a decisions endpoint. Keep `api = "decisions"` if you have a key for such a provider.

## Keys

- Prefer `api_key_env`: the key stays in your environment, and the file holds no secret.
- A file that holds an `api_key` must be readable by its owner only (mode 0600 on macOS and Linux); shuttle refuses to start otherwise.
- Keys stay in the daemon. Scripts call models through the daemon over the desk wire and never see a key; no log, error or `status` output contains one.

## Safety

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
