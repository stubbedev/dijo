#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <tessdata directory>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

digest() {
    local sum
    sum=$(sha256sum < "$1")
    echo "${sum%% *}"
}
dir=$1
# shellcheck source=ci/tessdata.env
source "$here/tessdata.env"
base="https://raw.githubusercontent.com/tesseract-ocr/tessdata_fast/$TESSDATA_COMMIT"

while IFS=$'\t' read -r model sha256; do
    case "$model" in
        '' | '#'*) continue ;;
    esac
    file="$dir/$model.traineddata"
    if [ -f "$file" ] && [ "$(digest "$file")" = "$sha256" ]; then
        continue
    fi
    curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location --retry 5 --max-time 600 \
        --create-dirs --output "$file.part" "$base/$model.traineddata"
    if [ "$(digest "$file.part")" != "$sha256" ]; then
        rm -f "$file.part"
        echo "$model does not have the pinned SHA-256 $sha256" >&2
        exit 1
    fi
    mv "$file.part" "$file"
done < "$here/tessdata.tsv"
