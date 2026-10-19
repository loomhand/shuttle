#!/bin/bash
# Builds the UI server's dex jar to <out>: javac against the stubs of the Android classes it calls, then r8's D8; needs a JDK and curl.
# usage: android/ui/build.sh <out.jar>
set -euo pipefail
OUT=${1:?usage: build.sh <out.jar>}
R8_URL=https://dl.google.com/android/maven2/com/android/tools/r8/9.5.26/r8-9.5.26.jar
R8_SHA256=3502f087388ce95bbf5254555927f09385dc2636873964d0bd53f3eaf4fb49ad
SRC="$(cd "$(dirname "$0")" && pwd)"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -fsSL --retry 3 --retry-connrefused "$R8_URL" -o "$tmp/r8.jar"
echo "$R8_SHA256  $tmp/r8.jar" | sha256sum -c --quiet -
mapfile -t stubs < <(find "$SRC/stubs" -name '*.java')
javac -nowarn --release 11 -d "$tmp/stubs" "${stubs[@]}"
javac -nowarn --release 11 -cp "$tmp/stubs" -d "$tmp/classes" "$SRC/src/shuttle/Ui.java"
java -cp "$tmp/r8.jar" com.android.tools.r8.D8 --min-api 30 --output "$tmp/shuttle-ui.jar" "$tmp/classes/shuttle/Ui.class"
install -D -m 0644 "$tmp/shuttle-ui.jar" "$OUT"
