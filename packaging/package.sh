#!/usr/bin/env bash
# Builds one release archive from the cocoon-shuttle binary in a cargo release directory, the runtime, and cua-driver and Node from packaging/pins;
# a linux archive also carries the Android UI server jar that UI_JAR names (android/ui/build.sh builds it).
set -euo pipefail

NAME=cocoon-shuttle
APP="Cocoon Shuttle"
BUNDLE_ID=io.github.loomhand.cocoon-shuttle

usage() {
	echo "usage: $0 PLATFORM RELEASE_DIR VERSION OUT_DIR; PLATFORM: macos-arm64, macos-x64, linux-x64, linux-arm64, windows-x64" >&2
	exit 2
}

[ $# -eq 4 ] || usage
PLATFORM=$1
BIN=$2/$NAME
VERSION=$3
ROOT=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$4"
OUT=$(cd "$4" && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# licenses DIR NODE CUA: the license of cocoon-shuttle and of each bundled component
licenses() {
	mkdir -p "$1"
	cp "$ROOT/LICENSE" "$1/LICENSE"
	cp "$2/LICENSE" "$1/node-LICENSE"
	cp "$3/LICENSE" "$1/cua-driver-LICENSE"
	cp "$3/THIRD_PARTY_NOTICES.md" "$1/cua-driver-THIRD_PARTY_NOTICES.md"
}

# runtime NODE DIR: the runtime sources and, next to them, their .mjs with the types stripped, which the runner prefers
runtime() {
	cp -R "$ROOT/runtime/lib" "$2"
	"$1" --disable-warning=ExperimentalWarning "$ROOT/packaging/runtime.mjs" "$2" "$2"
}

node=$("$ROOT/packaging/fetch.sh" "$PLATFORM" node "$WORK")
cua=$("$ROOT/packaging/fetch.sh" "$PLATFORM" cua-driver "$WORK")
stage="$WORK/stage"
archive="$OUT/$NAME-$VERSION-$PLATFORM"

case "$PLATFORM" in
macos-*)
	app="$stage/$APP.app"
	mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
	install -m 0755 "$BIN" "$app/Contents/MacOS/$NAME"
	install -m 0755 "$cua/cua-driver" "$cua/cua-cursor-theme" "$node/bin/node" "$app/Contents/MacOS/"
	sed -e "s/@NAME@/$NAME/" -e "s/@APP@/$APP/" -e "s/@BUNDLE_ID@/$BUNDLE_ID/" -e "s/@VERSION@/${VERSION#v}/" \
		"$ROOT/packaging/macos/Info.plist" >"$app/Contents/Info.plist"
	runtime "$node/bin/node" "$app/Contents/Resources/runtime"
	licenses "$app/Contents/Resources/licenses" "$node" "$cua"
	# node keeps its own signature; the unsigned x86_64 cua-driver binaries need one inside a signed bundle.
	codesign --force --sign - "$app/Contents/MacOS/cua-driver" "$app/Contents/MacOS/cua-cursor-theme"
	codesign --force --sign - "$app"
	codesign --verify --strict --verbose=2 "$app"
	COPYFILE_DISABLE=1 tar -czf "$archive.tar.gz" -C "$stage" "$APP.app"
	;;
linux-*)
	dir="$stage/$NAME"
	mkdir -p "$dir"
	install -m 0755 "$BIN" "$dir/$NAME"
	install -m 0755 "$cua/cua-driver" "$cua/cua-cursor-theme" "$node/bin/node" "$dir/"
	cp -R "$cua/wayland-helper" "$dir/wayland-helper"
	install -D -m 0644 "${UI_JAR:?set UI_JAR to the jar android/ui/build.sh builds}" "$dir/android/shuttle-ui.jar"
	runtime "$node/bin/node" "$dir/runtime"
	licenses "$dir/licenses" "$node" "$cua"
	tar -czf "$archive.tar.gz" -C "$stage" "$NAME"
	;;
windows-*)
	dir="$stage/$NAME"
	mkdir -p "$dir"
	cp "$BIN.exe" "$dir/$NAME.exe"
	cp "$cua/cua-driver.exe" "$cua/cua-cursor-theme.exe" "$node/node.exe" "$dir/"
	runtime "$node/node.exe" "$dir/runtime"
	licenses "$dir/licenses" "$node" "$cua"
	(cd "$stage" && 7z a -tzip -bso0 "$archive.zip" "$NAME")
	;;
*) usage ;;
esac
ls -l "$archive".*
