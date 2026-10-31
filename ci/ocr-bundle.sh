#!/usr/bin/env bash
set -euo pipefail

# The OCR bundle a build embeds through DIJO_OCR_BUNDLE: the language data, the PP-OCR models and their licences,
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
    "$here/ocr-vcpkg.sh" "$staging"
    "$here/fetch-models.sh" "$staging"
fi
rm -rf "$output"
mv "$staging" "$output"
