# Install

## Installers

macOS and Linux:

```sh
curl -fsSL https://github.com/loomhand/shuttle/releases/latest/download/install.sh | sh
```

| Variable | Default | |
|---|---|---|
| `SHUTTLE_VERSION` | the latest release | a tag such as `v0.0.1` |
| `SHUTTLE_PREFIX` | `~/.local` | `cocoon-shuttle` goes in `$SHUTTLE_PREFIX/bin`, the Linux files in `$SHUTTLE_PREFIX/lib/cocoon-shuttle` |
| `SHUTTLE_APP_DIR` | `~/Applications` | where `Cocoon Shuttle.app` goes on macOS |

Windows (PowerShell):

```powershell
irm https://github.com/loomhand/shuttle/releases/latest/download/install.ps1 | iex
```

It installs to `%LOCALAPPDATA%\Programs\cocoon-shuttle` (`SHUTTLE_HOME` overrides it) and adds that directory to your user `PATH`. `SHUTTLE_VERSION` picks a release.

Both installers download the archive and the release's `SHA256SUMS`, refuse an archive whose sha256 does not match, and need no administrator rights. Homebrew and Scoop packages follow with v0.1.0.

## What an archive holds

| | macOS (`Cocoon Shuttle.app`) | Linux and Windows (`cocoon-shuttle/`) |
|---|---|---|
| `cocoon-shuttle` | `Contents/MacOS/cocoon-shuttle` | `cocoon-shuttle`, `cocoon-shuttle.exe` |
| cua-driver | `Contents/MacOS/cua-driver`, `cua-cursor-theme` | `cua-driver`, `cua-cursor-theme` (Linux: and `wayland-helper/`) |
| Node.js 24 | `Contents/MacOS/node` | `node`, `node.exe` |
| runtime | `Contents/Resources/runtime/` | `runtime/` |
| licenses | `Contents/Resources/licenses/` | `licenses/` |

## Manual install and verification

Download an archive and `SHA256SUMS` from the [releases](https://github.com/loomhand/shuttle/releases), then:

```sh
shasum -a 256 -c SHA256SUMS --ignore-missing     # macOS; sha256sum -c on Linux
gh attestation verify cocoon-shuttle-v0.0.1-linux-x64.tar.gz -R loomhand/shuttle
```

The attestation shows that the archive was built by this repository's release workflow from the tagged commit. It is build provenance, not a code signature, and macOS and Windows do not read it.

## Uninstall

- macOS: `rm -rf ~/Applications/"Cocoon Shuttle.app" ~/.local/bin/cocoon-shuttle`, then remove Cocoon Shuttle from Privacy & Security.
- Linux: `rm -rf ~/.local/lib/cocoon-shuttle ~/.local/bin/cocoon-shuttle`
- Windows: `Remove-Item -Recurse "$env:LOCALAPPDATA\Programs\cocoon-shuttle"`, then remove that directory from your user `PATH`.

Remove `~/.config/cocoon-shuttle` (Windows: `%APPDATA%\cocoon-shuttle`) to drop the configuration too.
