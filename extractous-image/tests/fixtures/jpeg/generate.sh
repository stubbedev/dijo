#!/usr/bin/env bash
set -euo pipefail

# A YCbCr JPEG of a coloured test card; a YCCK one, as ImageMagick writes it, with an Adobe
# marker and inverted samples as Photoshop writes them; the same file with its Adobe transform set
# to 0, which makes it a CMYK JPEG; and the ink of each pixel of the last two as libjpeg-turbo
# decodes them for ImageMagick, as raw CMYK samples. Run with
# `nix shell nixpkgs#imagemagick --command extractous-image/tests/fixtures/jpeg/generate.sh`.

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

magick -size 96x80 gradient:red-blue -fill white -draw "rectangle 8,8 56,40" -fill black \
    -pointsize 20 -draw "text 12,32 'Ab'" "$work/card.png"
magick "$work/card.png" "$here/ycbcr.jpg"
magick "$work/card.png" -colorspace CMYK -sampling-factor 2x2,1x1,1x1,2x2 "$here/ycck.jpg"
cp "$here/ycck.jpg" "$here/cmyk.jpg"
adobe=$(grep -obUa 'Adobe' "$here/cmyk.jpg" | head -1 | cut -d: -f1)
printf '\000' | dd of="$here/cmyk.jpg" bs=1 seek=$((adobe + 11)) conv=notrunc status=none
for name in cmyk ycck; do
    magick "$here/$name.jpg" -depth 8 "cmyk:$here/$name.ink"
done
