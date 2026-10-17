# Development

## Build and test

```sh
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

The toolchain is pinned in `rust-toolchain.toml`. The unit tests drive fake accessibility backends, a fake cua-driver and in-memory MCP sessions: none reads a real accessibility tree or sends input, so they are safe on the machine you work on. Anything that drives a real desktop runs in a virtual machine or a CI runner, never on a developer's own session.

The Windows code builds only for Windows; from another host, `cargo clippy --all-targets --target x86_64-pc-windows-gnu -- -D warnings` checks it.

The runtime is TypeScript that Node runs with its types stripped; `npm ci && npm run typecheck` in `runtime/` checks the files `runtime/tsconfig.json` includes with `strict` and `noUncheckedIndexedAccess`; the remaining tests join as they are brought to that bar. The runtime tests run on Linux with the Node.js release the archives bundle:

```sh
sudo install -d -o "$USER" /workspace   # gui_plan's flows directory on Linux
node --test runtime/test/*.test.mts
```

CI runs the Rust gates on Linux (x64, arm64), macOS (arm64, x64) and Windows (x64), and the runtime tests and shellcheck on Linux.

## Release

1. Add a `## X.Y.Z - date` section to `CHANGELOG.md`; it becomes the release notes.
2. Set `version` in the workspace `Cargo.toml` and commit.
3. Tag `vX.Y.Z` and push the tag.

The release workflow builds the five archives, writes `SHA256SUMS`, attests their provenance and publishes them with the installers as a prerelease. It then installs that release with `install.sh` and `install.ps1` on fresh runners, asks each installed binary for `status` over MCP, and only then marks the release as the latest. A failed install job leaves it a prerelease: fix the cause and tag the next patch version; a published tag is never moved.

## Third-party pins

`packaging/pins` lists the cua-driver and Node.js archive of each platform with its sha256; `packaging/fetch.sh`, which `packaging/package.sh` and CI use, refuses an archive whose sha256 differs.

- **Node.js:** take the archive names and sums from `https://nodejs.org/dist/vX.Y.Z/SHASUMS256.txt`.
- **cua-driver:** tag `vX.Y.Z-loomhand.N` on [loomhand/cua](https://github.com/loomhand/cua); its `Fork: Release` workflow publishes the five archives with `SHA256SUMS`, and a published release is never overwritten.
