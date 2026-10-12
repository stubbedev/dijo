#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -lt 3 ]; then
    echo "usage: $0 <cache directory> <input directory> <output directory> [tika-app option...]" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
dir=$1
input=$2
output=$3
shift 3
version=$(sed -n 's/^def tikaVersion = "\(.*\)"$/\1/p' "$here/../extractous-core/tika-native/build.gradle")
sha256=$(awk -v version="$version" '$1 == version { print $3 }' "$here/pins.tsv")
if [ -z "$sha256" ]; then
    echo "conformance/pins.tsv pins no tika-app checksum for Tika $version" >&2
    exit 1
fi
mkdir -p "$dir"
archive="$dir/tika-app-$version.zip"
if [ ! -f "$archive" ]; then
    curl --fail --silent --show-error --location --max-time 1800 --output "$archive.partial" \
        "https://www.apache.org/dyn/closer.lua/tika/$version/tika-app-$version.zip?action=download"
    mv "$archive.partial" "$archive"
fi
echo "$sha256  $archive" | sha256sum --check --quiet
app="$dir/tika-app-$version"
if [ ! -d "$app" ]; then
    rm -rf "$app.partial"
    unzip -q "$archive" -d "$app.partial"
    mv "$app.partial" "$app"
fi

java -jar "$app/tika-app-$version.jar" --pluginsDir="$app/plugins" --handler t --content-only \
    --numClients 8 "$@" --inputDir="$input" --outputDir="$output"
