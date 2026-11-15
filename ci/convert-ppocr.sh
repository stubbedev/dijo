#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <output directory>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=ci/fetch-pinned.sh
source "$here/fetch-pinned.sh"
mkdir -p "$1"
output=$(cd "$1" && pwd)
work="$output/weights"

while IFS=$'\t' read -r model url sha256 _; do
    fetch_pinned "$url" "$sha256" "$work/$model/${url##*/}"
done < "$here/ppocr-weights.tsv"

quantise='
import sys
import onnx
from onnxruntime.quantization import QuantType, quantize_dynamic

source, target, op_types = sys.argv[1], sys.argv[2], sys.argv[3].split(",")
quantize_dynamic(source, target, op_types_to_quantize=op_types, weight_type=QuantType.QInt8, per_channel=True)
model = onnx.load(target)
for tensor in model.graph.initializer:
    tensor.ClearField("data_location")
onnx.save(model, target)
'

python3.12 -m venv "$work/venv"
"$work/venv/bin/pip" install --quiet paddlepaddle==3.3.1 paddle2onnx==2.1.0 packaging==25.0 onnxruntime==1.32.0
cut -f1,4,5 "$here/ppocr-weights.tsv" | sort -u | while read -r model opset quantised; do
    converted="$output/$model.onnx"
    if [ "$quantised" != - ]; then
        converted="$work/$model.onnx"
    fi
    (cd "$work" && "$work/venv/bin/paddle2onnx" --model_dir "$work/$model" --model_filename inference.json \
        --params_filename inference.pdiparams --opset_version "$opset" --save_file "$converted")
    if [ "$quantised" != - ]; then
        (cd "$work" && "$work/venv/bin/python" -I -c "$quantise" "$converted" "$output/$model.onnx" "$quantised")
    fi
    expected=$(awk -F'\t' -v name="$model.onnx" '$1 == name { print $3 }' "$here/ppocr.tsv")
    actual=$(sha256_of "$output/$model.onnx")
    if [ "$actual" != "$expected" ]; then
        echo "$output/$model.onnx has the SHA-256 $actual, not ci/ppocr.tsv's $expected" >&2
        exit 1
    fi
done
