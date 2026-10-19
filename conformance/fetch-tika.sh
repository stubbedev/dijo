#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ] || { [ "$2" != app ] && [ "$2" != server-standard ]; }; then
    echo "usage: $0 <cache directory> <app|server-standard>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
dir=$1
artifact=tika-$2
case "$2" in
    app) column=3 ;;
    server-standard) column=4 ;;
esac
version=$("$here/tika-version.sh")
sha256=$(awk -v version="$version" -v column="$column" '$1 == version { print $column }' "$here/pins.tsv")
if [ -z "$sha256" ]; then
    echo "conformance/pins.tsv pins no $artifact checksum for Tika $version" >&2
    exit 1
fi
mkdir -p "$dir"
archive="$dir/$artifact-$version.zip"
if [ ! -f "$archive" ]; then
    curl --fail --silent --show-error --location --max-time 1800 --output "$archive.partial" \
        "https://www.apache.org/dyn/closer.lua/tika/$version/$artifact-$version.zip?action=download"
    mv "$archive.partial" "$archive"
fi
echo "$sha256  $archive" | sha256sum --check --quiet >&2
unpacked="$dir/$artifact-$version"
if [ ! -d "$unpacked" ]; then
    rm -rf "$unpacked.partial"
    unzip -q "$archive" -d "$unpacked.partial"
    mv "$unpacked.partial" "$unpacked"
fi
echo "$unpacked"
