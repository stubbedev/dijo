#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <tessdata directory>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# shellcheck source=ci/fetch-pinned.sh
source "$here/fetch-pinned.sh"
dir=$1
# shellcheck source=ci/tessdata.env
source "$here/tessdata.env"
base="https://raw.githubusercontent.com/tesseract-ocr/tessdata_fast/$TESSDATA_COMMIT"

while IFS=$'\t' read -r model sha256; do
    case "$model" in
        '' | '#'*) continue ;;
    esac
    fetch_pinned "$base/$model.traineddata" "$sha256" "$dir/$model.traineddata"
done < "$here/tessdata.tsv"
