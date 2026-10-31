---
title: shuttle
---

# shuttle

shuttle (binary `shuttle`, app Shuttle) is a computer-use host that runs on your own machine. Local agents such as Claude Code, Codex and Cursor start it as an MCP server over stdio and drive your desktop through it: whole GUI plans in one call (`gui_plan`), scripts against a desk API (`script_run`), single accessibility-tree calls (`desk`) and screenshots. Model calls for target picking, grounding and checks go to an OpenAI-compatible endpoint and key you configure, OpenRouter by default (bring your own key).

shuttle is part of [loomhand](https://github.com/loomhand). Its input, capture and window layer is [cua-driver](https://github.com/loomhand/cua) (MIT).

**Status:** v0.1.36 is the current release. On macOS and Windows the daemon drives the desktop through the bundled cua-driver (`desk`, `screenshot`, `gui_plan`, `script_run`), calls models with your own key, and has its safety controls: kill switch and hotkey, indicator, confirm mode and app allowlist. On Linux (X11) the daemon drives the desktop over AT-SPI, with input through the bundled cua-driver, without the hotkey, indicator or confirm dialog. On Linux and macOS the daemon can drive an Android device over adb instead of the desktop. v0.1.0's Windows build needs the Visual C++ Redistributable; later releases do not. See [Platforms](platforms.md).

## Guides

- [Install](install.md): installers, first-time setup, verification, manual install, uninstall
- [Configuration](config.md): the model configuration (bring your own key) and the safety settings
- [Safety](safety.md): kill switch, indicator, confirm mode, allowlist, input per OS
- [Platforms](platforms.md): what exists per OS and the permissions each needs
- [Architecture](architecture.md): processes and components
- [Desk wire](wire.md): the desk wire between the host, the runtime and the desk backends
- [Development](development.md): building, testing and releasing

Source: [github.com/loomhand/shuttle](https://github.com/loomhand/shuttle)
