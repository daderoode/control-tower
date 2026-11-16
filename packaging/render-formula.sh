#!/bin/sh
# Render the Homebrew formula from its template to stdout.
# Usage: packaging/render-formula.sh <version> <base-url> <sha256-arm64>
# Apple silicon only: there is no Intel build to give a sha256 for.
set -eu

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <version> <base-url> <sha256-arm64>" >&2
  exit 2
fi
version=$1 base_url=$2 sha_arm=$3

echo "$version" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$' || { echo "invalid version: $version" >&2; exit 1; }
echo "$sha_arm" | grep -qE '^[0-9a-f]{64}$' || { echo "invalid sha256: $sha_arm" >&2; exit 1; }

sed -e "s|@VERSION@|$version|g" \
    -e "s|@BASE_URL@|$base_url|g" \
    -e "s|@SHA_ARM@|$sha_arm|g" \
    "$(dirname "$0")/control-tower.rb.in"
