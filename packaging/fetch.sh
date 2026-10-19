#!/usr/bin/env bash
# Downloads a third-party archive of packaging/pins, checks its sha256 and unpacks it; prints the unpacked top directory.
set -euo pipefail

[ $# -eq 3 ] || { echo "usage: $0 PLATFORM COMPONENT DIR" >&2; exit 2; }
line=$(awk -v p="$1" -v c="$2" '$1 == p && $2 == c' "$(dirname "$0")/pins")
[ -n "$line" ] || { echo "no $2 pin for $1" >&2; exit 1; }
sum=$(echo "$line" | cut -d' ' -f3)
url=$(echo "$line" | cut -d' ' -f4)
dir="$3/$2"
file="$3/$(basename "$url")"
mkdir -p "$dir"
curl -fsSL --retry 3 --retry-connrefused -o "$file" "$url"
if command -v sha256sum >/dev/null; then got=$(sha256sum "$file" | cut -d' ' -f1); else got=$(shasum -a 256 "$file" | cut -d' ' -f1); fi
[ "$got" = "$sum" ] || { echo "sha256 mismatch: $url" >&2; exit 1; }
case "$file" in
*.zip) unzip -q "$file" -d "$dir" ;;
*) tar -xf "$file" -C "$dir" ;;
esac
echo "$dir/$(ls "$dir")"
