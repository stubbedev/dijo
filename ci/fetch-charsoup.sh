#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <directory>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

digest() {
    local sum
    sum=$(sha256sum < "$1")
    echo "${sum%% *}"
}
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
            echo "ci/charsoup-jars.tsv pins $path, but the build uses Tika $tika" >&2
            exit 1
            ;;
    esac
    file="$dir/$(basename "$path")"
    if ! { [ -f "$file" ] && [ "$(digest "$file")" = "$sha256" ]; }; then
        curl --proto '=https' --tlsv1.2 --silent --show-error --fail --location --retry 5 --max-time 300 \
            --output "$file.part" "https://repo1.maven.org/maven2/$path"
        if [ "$(digest "$file.part")" != "$sha256" ]; then
            rm -f "$file.part"
            echo "$path does not have the pinned SHA-256 $sha256" >&2
            exit 1
        fi
        mv "$file.part" "$file"
    fi
done < "$here/charsoup-jars.tsv"
