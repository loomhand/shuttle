# Install

## Installers

macOS 13.5 or later (the oldest the bundled Node.js runs on; `install.sh` refuses an older macOS before it changes anything) and Linux:

```sh
curl -fsSL https://github.com/loomhand/shuttle/releases/latest/download/install.sh | sh
```

| Variable | Default | |
|---|---|---|
| `SHUTTLE_VERSION` | the latest release | a tag such as `v0.1.28` |
| `SHUTTLE_PREFIX` | `~/.local` | `shuttle` goes in `$SHUTTLE_PREFIX/bin`, the Linux files in `$SHUTTLE_PREFIX/lib/loomhand/shuttle` |
| `SHUTTLE_APP_DIR` | `~/Applications/Loomhand` | where `Shuttle.app` goes on macOS |

Windows (PowerShell):

```powershell
irm https://github.com/loomhand/shuttle/releases/latest/download/install.ps1 | iex
```

It installs to `%LOCALAPPDATA%\Programs\loomhand\shuttle` (`SHUTTLE_HOME` overrides it) and adds that directory to your user `PATH`. `SHUTTLE_VERSION` picks a release.

Both installers download the archive and the release's `SHA256SUMS`, refuse an archive whose sha256 does not match, and need no administrator rights. Both run the new `shuttle --version` and keep the previous install when it does not start: `install.sh` checks the new program before it replaces the installed one, `install.ps1` moves the previous install aside and puts it back. `install.ps1` also refuses to replace a running install and changes nothing, so quit a running daemon and its `cua-driver.exe` first (see [Uninstall](#uninstall)). There are no Homebrew or Scoop packages yet.

## First-time setup

1. **Install** with the installer above. `shuttle --version` should print the version; on macOS and Linux it lives in `~/.local/bin`, so that directory must be on your `PATH`.
2. **macOS permissions.** shuttle needs Accessibility and Screen Recording, granted to the app, not to your terminal: in System Settings > Privacy & Security, open Accessibility, click +, choose `~/Applications/Loomhand/Shuttle.app` and turn it on; do the same under Screen & System Audio Recording. Accessibility covers the window list, tree reads and element actions: without them those desk calls fail and name the missing grant. Screen Recording covers screenshots, the allowlist's masked ones included: without it they fail. Quit the daemon after granting (`pkill -f 'shuttle serve'`) so the next call starts it with the grants. Each upgrade needs the grants again ([platforms.md](platforms.md#macos) explains why).
3. **Your model key.** Create a key at <https://openrouter.ai/settings/keys> and put it in the configuration file, which on macOS is simplest since the daemon does not see your shell's environment:

   ```sh
   mkdir -p ~/.config/loomhand/shuttle
   (umask 077 && printf 'api_key = "sk-or-..."\n' > ~/.config/loomhand/shuttle/config.toml)
   ```

   On Windows the file is `%APPDATA%\loomhand\shuttle\config.toml`. The file must be readable by you only (mode 0600 on macOS and Linux). Any other OpenAI-compatible endpoint works too: set `base_url` (and `decide_api = "chat"` without a decisions endpoint), as [config.md](config.md#models-bring-your-own-key) describes.
4. **Safety settings for a first try.** Add these to the same file: they turn off the one tool that runs the agent's own code and limit the desk to one app, here TextEdit (Windows: `"notepad.exe"`). The kill hotkey is on by default: ⌃⌥⌘⎋ on macOS, Ctrl+Alt+Shift+Esc on Windows; `shuttle stop` does the same from a terminal.

   ```toml
   [tools]
   script_run = false

   [safety]
   apps = ["TextEdit"]
   ```

   [safety.md](safety.md) describes each control and its limits.
5. **Add shuttle to your agent:** `claude mcp add shuttle -- shuttle mcp` (Codex and Cursor: see the [README](https://github.com/loomhand/shuttle#use-it-from-an-agent)). The first tool call other than `status` starts the daemon.
6. **Check it:** ask the agent to call shuttle's `status` tool, which answers without the daemon, then to list your windows with the `desk` tool (verb `windows`), which starts it. With the allowlist above, only TextEdit's windows show.

## What an archive holds

| | macOS (`Shuttle.app`) | Linux and Windows (`shuttle/`) |
|---|---|---|
| `shuttle` | `Contents/MacOS/shuttle` | `shuttle`, `shuttle.exe` |
| cua-driver | `Contents/MacOS/cua-driver`, `cua-cursor-theme` | `cua-driver`, `cua-cursor-theme`, `.exe` on Windows (Linux: and `wayland-helper/`) |
| Node.js 24 | `Contents/MacOS/node` | `node`, `node.exe` |
| runtime (`.mts` sources and their stripped `.mjs`) | `Contents/Resources/runtime/` | `runtime/` |
| Android UI server | | `android/shuttle-ui.jar` (Linux only) |
| licenses | `Contents/Resources/licenses/` | `licenses/` |

## Embedding shuttle in an app

An app that ships shuttle inside its own bundle and starts `shuttle serve` itself, so that the macOS permissions belong to that app and not to `Shuttle.app`, sets `SHUTTLE_EMBEDDED=1` (exactly `1`) in the environment of both `shuttle serve` and the `shuttle mcp` its agents run:

- `serve` starts the bundled cua-driver with `CUA_DRIVER_EMBEDDED=1`, so the driver raises no permission prompts of its own; the app asks for Accessibility and Screen Recording.
- `mcp` only connects to the daemon. When none answers at the endpoint, it fails at once with `no shuttle daemon answers at <endpoint>` and starts nothing: the app keeps `shuttle serve` running.

Both use the default endpoint unless `SHUTTLE_SOCKET` names another one ([wire.md](wire.md)).

## Manual install and verification

Download an archive and `SHA256SUMS` from the [releases](https://github.com/loomhand/shuttle/releases), then:

```sh
shasum -a 256 -c SHA256SUMS --ignore-missing     # macOS; sha256sum -c on Linux
gh attestation verify shuttle-v0.1.28-linux-x64.tar.gz -R loomhand/shuttle
```

The attestation shows that the archive was built by this repository's release workflow from the tagged commit. It is build provenance, not a code signature, and macOS and Windows do not read it.

## Uninstall

- macOS: `rm -rf ~/Applications/Loomhand/Shuttle.app ~/.local/bin/shuttle`, then remove Shuttle from Privacy & Security.
- Linux: `rm -rf ~/.local/lib/loomhand/shuttle ~/.local/bin/shuttle`
- Windows: `Remove-Item -Recurse "$env:LOCALAPPDATA\Programs\loomhand\shuttle"`, then remove that directory from your user `PATH`.

Quit a running daemon first (`pkill -f 'shuttle serve'`; Windows: end `shuttle.exe` and `cua-driver.exe` in Task Manager, since ending the daemon there leaves its driver running from the install directory). Remove `~/.config/loomhand/shuttle` (Windows: `%APPDATA%\loomhand\shuttle`) to drop the configuration and the saved flows too, and `~/Library/Caches/loomhand/shuttle` on macOS or `$XDG_CACHE_HOME/loomhand/shuttle` (else `~/.cache/loomhand/shuttle`) on Linux (the daemon's sockets) or `%LOCALAPPDATA%\loomhand\shuttle` on Windows (the driver's pid file).
