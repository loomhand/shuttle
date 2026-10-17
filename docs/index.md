---
title: shuttle
---

# shuttle

shuttle (binary `cocoon-shuttle`, app Cocoon Shuttle) is a computer-use host that runs on your own machine. Local agents such as Claude Code, Codex and Cursor start it as an MCP server over stdio and drive your desktop through it: whole GUI plans in one call (`gui_plan`), scripts against a desk API (`script_run`), single accessibility-tree calls (`desk`) and screenshots. Model calls for target picking, grounding and checks go to an OpenAI-compatible endpoint and key you configure, OpenRouter by default (bring your own key).

shuttle is part of [loomhand](https://github.com/loomhand). It takes its desk core and plan executor from [cocoonbox](https://loomhand.github.io/cocoonbox/), where they drive cloud desktops, and its input, capture and window layer from [cua-driver](https://github.com/loomhand/cua) (MIT).

**Status:** v0.0.1, the latest release, is a skeleton: it installs, answers MCP on stdio and lists its five tools; only `status` runs. `main`, not yet released, adds the daemon, desk control on macOS and Windows (`desk`, `screenshot`, `gui_plan`, `script_run`, a developer preview that runs the bundled cua-driver) and bring-your-own-key models; the safety controls are not built yet. See [Platforms](platforms.md).

## Guides

- [Install](install.md): installers, verification, manual install, uninstall
- [Configuration](config.md): the model configuration (bring your own key)
- [Safety](safety.md): kill switch, indicator, confirm mode, allowlist, input per OS
- [Platforms](platforms.md): what exists per OS and the permissions each needs
- [Architecture](architecture.md): processes, components and the dependency plan with cocoonbox
- [Desk wire](wire.md): the desk wire between the host, the runtime and the desk backends
- [Development](development.md): building, testing and releasing

Source: [github.com/loomhand/shuttle](https://github.com/loomhand/shuttle)
