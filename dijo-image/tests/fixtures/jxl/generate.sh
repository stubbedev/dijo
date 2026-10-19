#!/usr/bin/env bash
set -euo pipefail

# JPEG XL images: the Danish scan of the quality corpus, losslessly, for OCR to read as it reads
# the PNG; and a small JPEG of a test card with Exif (camera make, orientation 6) and XMP (a
# title), recompressed losslessly, its metadata boxes Brotli-compressed, the orientation carried
# into the codestream's header. Run with
# `nix shell nixpkgs#libjxl nixpkgs#imagemagick nixpkgs#exiftool --command
# dijo-image/tests/fixtures/jxl/generate.sh`.

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
corpus=$(cd "$here/../../../../quality/corpus" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cjxl -d 0 -e 7 "$corpus/danish-scan.png" "$here/scan.jxl"

magick -size 96x80 gradient:red-blue -fill white -draw "rectangle 8,8 56,40" -fill black \
    -pointsize 20 -draw "text 12,32 'Ab'" "$work/card.jpg"
exiftool -q -overwrite_original -Make=Dijo -Orientation#=6 -XMP-dc:Title=Fyret \
    "$work/card.jpg"
cjxl --compress_boxes=1 "$work/card.jpg" "$here/card.jxl"
