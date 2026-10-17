#!/usr/bin/env bash
set -euo pipefail

# The AVIF images the native decoder is checked against libheif with: a coloured test card
# coded with each layout, range, depth, matrix and transformative property it reads. Run with
# `nix shell nixpkgs#imagemagick nixpkgs#libavif --command extractous-image/tests/fixtures/avif/generate.sh`.

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

magick -size 128x128 gradient:red-blue \( -size 128x128 gradient:yellow-black -rotate 90 \) \
    -compose multiply -composite -fill white -draw "rectangle 8,8 56,40" -fill black \
    -pointsize 20 -draw "text 12,32 'Ab'" "$work/card.png"
magick "$work/card.png" -crop 96x80+0+0 +repage "$work/small.png"

avifenc --speed 9 --yuv 420 --range limited --cicp 1/13/1 "$work/small.png" "$here/limited-709-420.avif"
avifenc --speed 9 --yuv 444 --range full --cicp 1/13/6 --depth 10 "$work/small.png" "$here/full-601-444-10bit.avif"
avifenc --speed 9 --yuv 422 --cicp 1/13/9 "$work/small.png" "$here/bt2020-422.avif"
avifenc --speed 9 --yuv 400 "$work/small.png" "$here/monochrome.avif"
avifenc --speed 9 --yuv 444 --cicp 1/13/0 "$work/small.png" "$here/identity-444.avif"
avifenc --speed 9 --grid 2x2 "$work/card.png" "$here/grid.avif"
avifenc --speed 9 --irot 1 --imir 0 "$work/small.png" "$here/rotated-mirrored.avif"
avifenc --speed 9 --imir 1 "$work/small.png" "$here/mirrored.avif"
avifenc --speed 9 --crop 10,6,64,48 "$work/small.png" "$here/cropped.avif"
