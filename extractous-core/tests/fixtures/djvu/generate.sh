#!/usr/bin/env bash
set -euo pipefail

# The DjVu pages the renderer is checked against our Java DjvuRenderer with, beyond the corpora's:
# IW44 backgrounds in colour and in grey, coded in several chunks, a page whose JB2 mask is coloured
# by an IW44 foreground, a JPEG background, and turned pages. Run with
# `nix shell nixpkgs#imagemagick nixpkgs#djvulibre --command extractous-core/tests/fixtures/djvu/generate.sh`,
# then `just djvu-reference`.

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

magick -size 300x200 gradient:orange-navy \( -size 300x200 radial-gradient:white-teal \) \
    -compose overlay -composite -fill white -draw "rectangle 20,20 140,80" -fill black \
    -draw "rectangle 30,35 40,65" -draw "circle 60,50 70,50" -draw "polygon 85,65 95,35 105,65" \
    "$work/card.ppm"
magick "$work/card.ppm" -colorspace Gray "$work/card.pgm"
magick -size 300x200 xc:white -fill black -draw "rectangle 20,80 30,130" \
    -draw "circle 60,110 75,110" -draw "polygon 90,130 105,80 120,130" -draw "rectangle 140,80 150,130" \
    -draw "circle 180,110 195,110" -draw "ellipse 240,105 20,25 0,360" "$work/text.pbm"
magick -size 100x67 gradient:khaki-sienna "$work/background.ppm"
magick -size 100x67 gradient:red-blue "$work/foreground.ppm"
magick -size 150x100 gradient:lavender-gray -quality 80 "$work/background.jpg"

c44 -slice 74,89,99 "$work/card.ppm" "$here/iw44-colour.djvu"
c44 -slice 72,84,90 "$work/card.pgm" "$here/iw44-grey.djvu"

cjb2 "$work/text.pbm" "$work/mask.djvu"
djvuextract "$work/mask.djvu" Sjbz="$work/mask.sjbz"
c44 "$work/background.ppm" "$work/background.djvu"
djvuextract "$work/background.djvu" BG44="$work/background.bg44"
c44 -slice 100 "$work/foreground.ppm" "$work/foreground.djvu"
djvuextract "$work/foreground.djvu" BG44="$work/foreground.fg44"
djvumake "$here/compound.djvu" INFO=300,200,300 Sjbz="$work/mask.sjbz" \
    FG44="$work/foreground.fg44" BG44="$work/background.bg44"
djvumake "$here/jpeg-background.djvu" INFO=300,200,300 Sjbz="$work/mask.sjbz" \
    BGjp="$work/background.jpg"

cp "$here/compound.djvu" "$here/turned-clockwise.djvu"
djvused -s -e 'select 1; set-rotation 3' "$here/turned-clockwise.djvu"
cp "$here/iw44-grey.djvu" "$here/upside-down.djvu"
djvused -s -e 'select 1; set-rotation 2' "$here/upside-down.djvu"
