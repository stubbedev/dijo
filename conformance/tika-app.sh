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
version=$("$here/../extractous-core/tika-native/tika-version.sh")
app=$("$here/fetch-tika.sh" "$dir" app)

java -jar "$app/tika-app-$version.jar" --pluginsDir="$app/plugins" --handler t --content-only \
    --numClients 8 "$@" --inputDir="$input" --outputDir="$output"
