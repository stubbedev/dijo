#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <directory>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# shellcheck source=ci/fetch-pinned.sh
source "$here/fetch-pinned.sh"
dir=$1
tika=$("$here/../extractous-core/tika-native/tika-version.sh")
mkdir -p "$dir"

while IFS=$'\t' read -r path sha256; do
    case "$path" in
        '' | '#'*) continue ;;
    esac
    case "$path" in
        org/apache/tika/*/"$tika"/*) ;;
        org/apache/tika/*)
            echo "ci/reference-jars.tsv pins $path, but the build uses Tika $tika" >&2
            exit 1
            ;;
    esac
    case "$path" in
        *-sources.jar) target="$dir/sources" ;;
        *) target="$dir" ;;
    esac
    fetch_pinned "https://repo1.maven.org/maven2/$path" "$sha256" "$target/$(basename "$path")"
done < "$here/reference-jars.tsv"
