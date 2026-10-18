# Install

## Installers

macOS 13.5 or later (the oldest the bundled Node.js runs on; `install.sh` refuses an older macOS before it changes anything) and Linux:

```sh
curl -fsSL https://github.com/loomhand/shuttle/releases/latest/download/install.sh | sh
```

| Variable | Default | |
|---|---|---|
| `SHUTTLE_VERSION` | the latest release | a tag such as `v0.1.0` |
| `SHUTTLE_PREFIX` | `~/.local` | `cocoon-shuttle` goes in `$SHUTTLE_PREFIX/bin`, the Linux files in `$SHUTTLE_PREFIX/lib/cocoon-shuttle` |
| `SHUTTLE_APP_DIR` | `~/Applications` | where `Cocoon Shuttle.app` goes on macOS |

Windows (PowerShell):

```powershell
irm https://github.com/loomhand/shuttle/releases/latest/download/install.ps1 | iex
```

It installs to `%LOCALAPPDATA%\Programs\cocoon-shuttle` (`SHUTTLE_HOME` overrides it) and adds that directory to your user `PATH`. `SHUTTLE_VERSION` picks a release.

Both installers download the archive and the release's `SHA256SUMS`, refuse an archive whose sha256 does not match, and need no administrator rights. There are no Homebrew or Scoop packages yet.

## First-time setup

1. **Install** with the installer above. `cocoon-shuttle --version` should print the version; on macOS and Linux it lives in `~/.local/bin`, so that directory must be on your `PATH`.
2. **macOS permissions.** shuttle needs Accessibility and Screen Recording, granted to the app, not to your terminal: in System Settings > Privacy & Security, open Accessibility, click +, choose `~/Applications/Cocoon Shuttle.app` and turn it on; do the same under Screen & System Audio Recording. A desk call made before that fails and names the missing permission. Quit the daemon after granting (`pkill -f 'cocoon-shuttle serve'`) so the next call starts it with the grants. Each upgrade needs the grants again ([platforms.md](platforms.md#macos) explains why).
3. **Your model key.** Create a key at <https://openrouter.ai/settings/keys> and put it in the configuration file, which on macOS is simplest since the daemon does not see your shell's environment:

   ```sh
   mkdir -p ~/.config/cocoon-shuttle
   umask 077 && printf 'api_key = "sk-or-..."\n' > ~/.config/cocoon-shuttle/config.toml
   ```

   On Windows the file is `%APPDATA%\cocoon-shuttle\config.toml`. The file must be readable by you only (mode 0600 on macOS and Linux). Any other OpenAI-compatible endpoint works too: set `base_url` (and `decide_api = "chat"` without a decisions endpoint), as [config.md](config.md#models-bring-your-own-key) describes.
4. **Safety settings for a first try.** Add these to the same file: they turn off the one tool that runs the agent's own code and limit the desk to one app, here TextEdit (Windows: `"notepad.exe"`). The kill hotkey is on by default: ⌃⌥⌘⎋ on macOS, Ctrl+Alt+Shift+Esc on Windows; `cocoon-shuttle stop` does the same from a terminal.

   ```toml
   [tools]
   script_run = false

   [safety]
   apps = ["TextEdit"]
   ```

   [safety.md](safety.md) describes each control and its limits.
5. **Add shuttle to your agent:** `claude mcp add shuttle -- cocoon-shuttle mcp` (Codex and Cursor: see the [README](https://github.com/loomhand/shuttle#use-it-from-an-agent)). The first tool call starts the daemon.
6. **Check it:** ask the agent to call shuttle's `status` tool, then to list your windows with the `desk` tool (verb `windows`). With the allowlist above, only TextEdit's windows show.

## What an archive holds

| | macOS (`Cocoon Shuttle.app`) | Linux and Windows (`cocoon-shuttle/`) |
|---|---|---|
| `cocoon-shuttle` | `Contents/MacOS/cocoon-shuttle` | `cocoon-shuttle`, `cocoon-shuttle.exe` |
| cua-driver | `Contents/MacOS/cua-driver`, `cua-cursor-theme` | `cua-driver`, `cua-cursor-theme`, `.exe` on Windows (Linux: and `wayland-helper/`) |
| Node.js 24 | `Contents/MacOS/node` | `node`, `node.exe` |
| runtime | `Contents/Resources/runtime/` | `runtime/` |
| licenses | `Contents/Resources/licenses/` | `licenses/` |

## Manual install and verification

Download an archive and `SHA256SUMS` from the [releases](https://github.com/loomhand/shuttle/releases), then:

```sh
shasum -a 256 -c SHA256SUMS --ignore-missing     # macOS; sha256sum -c on Linux
gh attestation verify cocoon-shuttle-v0.1.0-linux-x64.tar.gz -R loomhand/shuttle
```

The attestation shows that the archive was built by this repository's release workflow from the tagged commit. It is build provenance, not a code signature, and macOS and Windows do not read it.

## Uninstall

- macOS: `rm -rf ~/Applications/"Cocoon Shuttle.app" ~/.local/bin/cocoon-shuttle`, then remove Cocoon Shuttle from Privacy & Security.
- Linux: `rm -rf ~/.local/lib/cocoon-shuttle ~/.local/bin/cocoon-shuttle`
- Windows: `Remove-Item -Recurse "$env:LOCALAPPDATA\Programs\cocoon-shuttle"`, then remove that directory from your user `PATH`.

Quit a running daemon first (`pkill -f 'cocoon-shuttle serve'`; Windows: end `cocoon-shuttle.exe` in Task Manager). Remove `~/.config/cocoon-shuttle` (Windows: `%APPDATA%\cocoon-shuttle`) to drop the configuration and the saved flows too, and on macOS `~/Library/Caches/cocoon-shuttle` (the daemon's socket).
