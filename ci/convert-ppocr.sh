#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <output directory>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=ci/fetch-pinned.sh
source "$here/fetch-pinned.sh"
output=$1
work="$output/weights"

while IFS=$'\t' read -r model url sha256; do
    fetch_pinned "$url" "$sha256" "$work/$model/${url##*/}"
done < "$here/ppocr-weights.tsv"

python3.12 -m venv "$work/venv"
"$work/venv/bin/pip" install --quiet paddlepaddle==3.3.1 paddle2onnx==2.1.0 packaging==25.0
cut -f1 "$here/ppocr-weights.tsv" | sort -u | while read -r model; do
    "$work/venv/bin/paddle2onnx" --model_dir "$work/$model" --model_filename inference.json \
        --params_filename inference.pdiparams --opset_version 14 --save_file "$output/$model.onnx"
    expected=$(awk -F'\t' -v name="$model.onnx" '$1 == name { print $3 }' "$here/ppocr.tsv")
    actual=$(sha256_of "$output/$model.onnx")
    if [ "$actual" != "$expected" ]; then
        echo "$output/$model.onnx has the SHA-256 $actual, not ci/ppocr.tsv's $expected" >&2
        exit 1
    fi
done
