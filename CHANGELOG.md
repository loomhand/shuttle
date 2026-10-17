# Changelog

## 0.0.1 - 2026-10-17

- One cargo workspace: `shuttle-desk-core` (the desk wire, call arguments and window matching) and `shuttle-desk` (the macOS AX and Windows UIA desk verbs) from cocoonbox with their history, and the `shuttle` binary.
- `cocoon-shuttle mcp` answers MCP on stdio and lists `gui_plan`, `script_run`, `desk`, `screenshot` and `status`; only `status` runs in this release.
- `cocoon-shuttle serve` and `cocoon-shuttle desk VERB` serve and call the desk verbs on macOS and Windows, a developer preview that needs a running cua-driver daemon.
- The desk runtime (`desk.mts`, `gui_plan.mts`) and its tests, unchanged from cocoonbox apart from test import paths.
- Release archives for macOS (arm64, x64), Windows (x64) and Linux (x64, arm64) with cua-driver v0.35.4-loomhand.2 and Node.js 24.22.0, both checked by sha256 at build time; `SHA256SUMS` and build provenance attestations.
- `install.sh` and `install.ps1`.
