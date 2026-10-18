#!/usr/bin/env bash
set -euo pipefail

# A JBIG2 file of two pages, the first and the second paragraph of the quality corpus's scan,
# coded by jbig2enc with a symbol dictionary they share, and its first page as jbig2dec decodes
# it; and a page of 8000x7600 pixels, more than OCR prepares, with a word on it. Run with `nix shell nixpkgs#imagemagick nixpkgs#jbig2enc nixpkgs#jbig2dec --command
# extractous-image/tests/fixtures/jbig2/generate.sh`.

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
scan="$here/../../../../quality/corpus/prose-scan.pbm"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

magick "$scan" -crop 2481x360+0+340 +repage "$work/first.png"
magick "$scan" -crop 2481x360+0+700 +repage "$work/second.png"
jbig2 -s "$work/first.png" "$work/second.png" > "$here/two-pages.jb2"
jbig2dec -q -t pbm -o "$work/pages.pbm" "$here/two-pages.jb2"
magick "$work/pages.pbm[0]" "$here/first-page.png"
magick -size 8000x7600 xc:white -fill black -pointsize 200 -draw "text 400,600 'Lighthouse'" \
    -monochrome "$work/large.png"
jbig2 "$work/large.png" > "$here/large.jb2"
