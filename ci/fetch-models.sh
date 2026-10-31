#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <OCR bundle directory>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# shellcheck source=ci/fetch-pinned.sh
source "$here/fetch-pinned.sh"
bundle=$1
# shellcheck source=ci/tessdata.env
source "$here/tessdata.env"
# shellcheck source=ci/ppocr.env
source "$here/ppocr.env"

tessdata="https://raw.githubusercontent.com/tesseract-ocr/tessdata_fast/$TESSDATA_COMMIT"
fetch_pinned "$tessdata/LICENSE" "$TESSDATA_LICENSE_SHA256" "$bundle/licenses/tessdata_fast/LICENSE"
while IFS=$'\t' read -r model sha256; do
    case "$model" in
        '' | '#'*) continue ;;
    esac
    fetch_pinned "$tessdata/$model.traineddata" "$sha256" "$bundle/tessdata/$model.traineddata"
done < "$here/tessdata.tsv"

fetch_pinned "https://raw.githubusercontent.com/PaddlePaddle/PaddleOCR/$PADDLEOCR_COMMIT/LICENSE" \
    "$PADDLEOCR_LICENSE_SHA256" "$bundle/licenses/PaddleOCR/LICENSE"
while IFS=$'\t' read -r name url sha256; do
    fetch_pinned "$url" "$sha256" "$bundle/ppocr/$name"
done < "$here/ppocr.tsv"
