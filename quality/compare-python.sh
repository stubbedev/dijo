#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <cache directory> <tools directory>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
cache=$(realpath "$1")
output=$(realpath "$2")
image=python:3.12-slim-bookworm@sha256:34386ef0cb081344d7ec1c103ba398e6e9f64e9ab3a1509accc92a4e24a07258
mkdir -p "$cache" "$output"

# shellcheck disable=SC2016
docker run --rm \
    -v "$here:/quality:ro" -v "$cache:/root/.cache" -v "$output:/out" \
    -e TESSDATA_PREFIX=/usr/share/tesseract-ocr/5/tessdata \
    -e EASYOCR_MODULE_PATH=/root/.cache/easyocr \
    -e OWNER="$(id -u):$(id -g)" \
    "$image" bash -euo pipefail -c '
        chown -R root:root /root/.cache
        trap "chown -R \"\$OWNER\" /out /root/.cache" EXIT
        apt-get update -qq
        apt-get install -y -qq --no-install-recommends tesseract-ocr-eng > /dev/null
        pip install -q --root-user-action=ignore --disable-pip-version-check \
            --extra-index-url https://download.pytorch.org/whl/cpu \
            kreuzberg==4.10.4 "docling[easyocr]==2.131.0" \
            "docling-slim[format-opendocument]==2.131.0"
        for tool in kreuzberg docling; do
            python /quality/compare_python.py "$tool" /quality/corpus /quality/corpus.tsv /out
        done
    '
