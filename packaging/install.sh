#!/bin/sh
# Installs cocoon-shuttle on macOS or Linux from a GitHub release, after checking the archive against the release's SHA256SUMS.
# usage: curl -fsSL https://github.com/loomhand/shuttle/releases/latest/download/install.sh | sh
# env: SHUTTLE_VERSION (a tag, default the latest release), SHUTTLE_PREFIX (default ~/.local), SHUTTLE_APP_DIR (macOS, default ~/Applications)
set -eu

NAME=cocoon-shuttle
APP="Cocoon Shuttle"
REPO=loomhand/shuttle
PREFIX=${SHUTTLE_PREFIX:-$HOME/.local}
APP_DIR=${SHUTTLE_APP_DIR:-$HOME/Applications}

say() { printf '%s\n' "$*"; }
die() { printf '%s: %s\n' "$NAME install" "$*" >&2; exit 1; }

sha256() {
	if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

command -v curl >/dev/null 2>&1 || die "curl is required"
command -v tar >/dev/null 2>&1 || die "tar is required"

case "$(uname -s)" in
Darwin) os=macos ;;
Linux) os=linux ;;
*) die "unsupported OS $(uname -s); on Windows use install.ps1" ;;
esac
case "$(uname -m)" in
arm64 | aarch64) arch=arm64 ;;
x86_64 | amd64) arch=x64 ;;
*) die "unsupported architecture $(uname -m)" ;;
esac
# a shell under Rosetta reports x86_64 on Apple silicon
if [ "$os" = macos ] && [ "$arch" = x64 ] && [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = 1 ]; then
	arch=arm64
fi

version=${SHUTTLE_VERSION:-}
if [ -z "$version" ]; then
	latest=$(curl -fsSLo /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") || die "cannot reach github.com"
	version=${latest##*/}
	case "$version" in v*) ;; *) die "no release found at https://github.com/$REPO/releases" ;; esac
fi
asset="$NAME-$version-$os-$arch.tar.gz"
base="https://github.com/$REPO/releases/download/$version"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
say "downloading $asset"
curl -fsSL --retry 3 -o "$tmp/$asset" "$base/$asset" || die "no $asset in release $version"
curl -fsSL --retry 3 -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || die "no SHA256SUMS in release $version"
want=$(awk -v a="$asset" '$2 == a { print $1 }' "$tmp/SHA256SUMS")
[ -n "$want" ] || die "SHA256SUMS lists no $asset"
[ "$(sha256 "$tmp/$asset")" = "$want" ] || die "sha256 mismatch for $asset"
say "sha256 ok"
tar -xzf "$tmp/$asset" -C "$tmp"

mkdir -p "$PREFIX/bin"
if [ "$os" = macos ]; then
	mkdir -p "$APP_DIR"
	rm -rf "${APP_DIR:?}/$APP.app"
	mv "$tmp/$APP.app" "$APP_DIR/$APP.app"
	xattr -dr com.apple.quarantine "$APP_DIR/$APP.app" 2>/dev/null || true
	ln -sf "$APP_DIR/$APP.app/Contents/MacOS/$NAME" "$PREFIX/bin/$NAME"
	home="$APP_DIR/$APP.app"
else
	mkdir -p "$PREFIX/lib"
	rm -rf "${PREFIX:?}/lib/$NAME"
	mv "$tmp/$NAME" "$PREFIX/lib/$NAME"
	ln -sf "$PREFIX/lib/$NAME/$NAME" "$PREFIX/bin/$NAME"
	home="$PREFIX/lib/$NAME"
fi

say "installed $("$PREFIX/bin/$NAME" --version) in $home"
case ":$PATH:" in
*":$PREFIX/bin:"*) ;;
*) say "add $PREFIX/bin to PATH, e.g. echo 'export PATH=\"$PREFIX/bin:\$PATH\"' >> ~/.profile" ;;
esac
say "register it with an agent, e.g.: claude mcp add shuttle -- $PREFIX/bin/$NAME mcp"
if [ "$os" = macos ]; then
	say "macOS: when $APP starts acting on the desktop, grant it Accessibility and Screen Recording in"
	say "System Settings > Privacy & Security, adding $home; an upgrade replaces the ad hoc signature, so grant it again after each upgrade"
fi
