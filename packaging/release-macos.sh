#!/usr/bin/env bash
# Replaces the macOS archives of a prerelease with Developer ID signed and notarized ones, rewrites its SHA256SUMS, then starts the release-verify workflow that installs and promotes it.
# usage: packaging/release-macos.sh TAG; needs gh and the environment packaging/sign-macos.sh reads
set -euo pipefail

REPO=loomhand/shuttle
ROOT=$(cd "$(dirname "$0")/.." && pwd)
[ $# -eq 1 ] || { echo "usage: $0 TAG" >&2; exit 2; }
TAG=$1

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

[ "$(gh release view "$TAG" -R "$REPO" --json isPrerelease --jq .isPrerelease)" = true ] || {
	echo "$TAG is not a prerelease; a promoted release is never changed" >&2
	exit 1
}
gh release download "$TAG" -R "$REPO" -D "$WORK" -p "shuttle-$TAG-macos-*.tar.gz" -p SHA256SUMS
(cd "$WORK" && shasum -a 256 -c SHA256SUMS --ignore-missing)
"$ROOT/packaging/sign-macos.sh" "$WORK"/shuttle-"$TAG"-macos-*.tar.gz
(
	cd "$WORK"
	for a in shuttle-"$TAG"-macos-*.tar.gz; do
		awk -v a="$a" -v s="$(shasum -a 256 "$a" | cut -d' ' -f1)" '$2 == a { print s "  " a; next } { print }' SHA256SUMS >SHA256SUMS.new
		mv SHA256SUMS.new SHA256SUMS
	done
	shasum -a 256 -c SHA256SUMS --ignore-missing
)
gh release upload "$TAG" -R "$REPO" --clobber "$WORK"/shuttle-"$TAG"-macos-*.tar.gz "$WORK/SHA256SUMS"
gh workflow run release-verify.yml -R "$REPO" -f tag="$TAG"
echo "uploaded; release-verify started for $TAG"
