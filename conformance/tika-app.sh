#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <directory>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
dir=$1
version=$(sed -n 's/^def tikaVersion = "\(.*\)"$/\1/p' "$here/../extractous-core/tika-native/build.gradle")
sha256=$(awk -v version="$version" '$1 == version { print $3 }' "$here/pins.tsv")
if [ -z "$sha256" ]; then
    echo "conformance/pins.tsv pins no tika-app checksum for Tika $version" >&2
    exit 1
fi
mkdir -p "$dir"
jar="$dir/tika-app-$version.jar"
if [ ! -f "$jar" ]; then
    curl --fail --silent --show-error --location --max-time 600 --output "$jar.partial" \
        "https://repo1.maven.org/maven2/org/apache/tika/tika-app/$version/tika-app-$version.jar"
    mv "$jar.partial" "$jar"
fi
echo "$sha256  $jar" | sha256sum --check --quiet
echo "$jar"
