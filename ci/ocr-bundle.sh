#!/usr/bin/env bash
set -euo pipefail

# The OCR bundle a build embeds through EXTRACTOUS_OCR_BUNDLE: the language data and its licence,
# and either tesseract as a library the process loads with heif-png beside it (ci/ocr-vcpkg.sh),
# or, with --linked, the licences of the tesseract and libraries the static executable links
# (ci/ocr-static.nix, which builds that whole bundle, Linux only).

usage() {
    echo "usage: $0 [--linked] <output directory>" >&2
    exit 2
}

linked=false
if [ "${1:-}" = --linked ]; then
    linked=true
    shift
fi
if [ "$#" -ne 1 ]; then
    usage
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
output=$1

staging="$output.partial"
rm -rf "$staging"
if "$linked"; then
    if [ "$(uname -s)" != Linux ]; then
        echo "Only the Linux static executable links tesseract" >&2
        exit 1
    fi
    bundle=$(nix build --no-link --print-out-paths --impure --expr \
        "import $here/ocr-static.nix { nixpkgs = import $here/nixpkgs.nix; system = builtins.currentSystem; }")
    mkdir -p "$staging"
    cp -R "$bundle/." "$staging/"
    chmod -R u+w "$staging"
else
    # shellcheck source=ci/fetch-pinned.sh
    source "$here/fetch-pinned.sh"
    # shellcheck source=ci/tessdata.env
    source "$here/tessdata.env"
    "$here/ocr-vcpkg.sh" "$staging"
    fetch_pinned "https://raw.githubusercontent.com/tesseract-ocr/tessdata_fast/$TESSDATA_COMMIT/LICENSE" \
        "$TESSDATA_LICENSE_SHA256" "$staging/licenses/tessdata_fast/LICENSE"
    "$here/fetch-tessdata.sh" "$staging/tessdata"
fi
rm -rf "$output"
mv "$staging" "$output"
