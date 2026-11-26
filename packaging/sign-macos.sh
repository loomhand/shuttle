#!/usr/bin/env bash
# Signs the Shuttle.app in each macOS archive with a Developer ID, notarizes it, staples the ticket and rewrites the archive in place.
# usage: packaging/sign-macos.sh ARCHIVE...; env: SIGN_APP (codesign identity, default "Developer ID Application"), NOTARY_KEY (App Store Connect API key, .p8), NOTARY_KEY_ID, NOTARY_ISSUER
set -euo pipefail

SIGN_APP=${SIGN_APP:-Developer ID Application}
DIR=$(cd "$(dirname "$0")/macos" && pwd)
: "${NOTARY_KEY:?set NOTARY_KEY to the App Store Connect API key (.p8)}"
: "${NOTARY_KEY_ID:?set NOTARY_KEY_ID}"
: "${NOTARY_ISSUER:?set NOTARY_ISSUER}"
[ $# -gt 0 ] || { echo "usage: $0 ARCHIVE..." >&2; exit 2; }

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

sign() {
	codesign --force --timestamp --options runtime --sign "$SIGN_APP" "$@"
}

for archive in "$@"; do
	rm -rf "${WORK:?}"/*
	tar -xzf "$archive" -C "$WORK"
	app="$WORK/Shuttle.app"
	bin="$app/Contents/MacOS"
	sign --entitlements "$DIR/apple-events.entitlements" "$bin/cua-driver"
	sign "$bin/cua-cursor-theme"
	sign --entitlements "$DIR/node.entitlements" "$bin/node"
	sign --entitlements "$DIR/apple-events.entitlements" "$app"
	codesign --verify --strict --deep --verbose=2 "$app"
	ditto -c -k --keepParent "$app" "$WORK/notarize.zip"
	xcrun notarytool submit "$WORK/notarize.zip" --key "$NOTARY_KEY" --key-id "$NOTARY_KEY_ID" --issuer "$NOTARY_ISSUER" \
		--wait --output-format plist >"$WORK/notary.plist"
	status=$(plutil -extract status raw "$WORK/notary.plist")
	id=$(plutil -extract id raw "$WORK/notary.plist")
	[ "$status" = Accepted ] || { echo "notarization of $archive: $status (xcrun notarytool log $id …)" >&2; exit 1; }
	rm "$WORK/notarize.zip" "$WORK/notary.plist"
	xcrun stapler staple "$app"
	spctl --assess --type execute --verbose=2 "$app"
	COPYFILE_DISABLE=1 tar -czf "$archive" -C "$WORK" Shuttle.app
	echo "signed, notarized and stapled: $archive"
done
