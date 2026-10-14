#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <output directory>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
output=$1
# shellcheck source=ci/tessdata.env
source "$here/tessdata.env"

staging="$output.partial"
rm -rf "$staging"
case "$(uname -s)" in
    Linux)
        rev=$(nix eval --raw --impure --expr \
            "(builtins.fromJSON (builtins.readFile $here/../devenv.lock)).nodes.nixpkgs.locked.rev")
        bundle=$(nix build --no-link --print-out-paths --impure --expr \
            "import $here/ocr-static.nix { nixpkgs = builtins.getFlake \"github:cachix/devenv-nixpkgs/$rev\"; system = builtins.currentSystem; }")
        mkdir -p "$staging"
        cp -r "$bundle/." "$staging/"
        chmod -R u+w "$staging"
        ;;
    Darwin | MINGW* | MSYS*)
        "$here/ocr-vcpkg.sh" "$staging"
        ;;
    *)
        echo "No tesseract build for $(uname -s)" >&2
        exit 1
        ;;
esac
mkdir -p "$staging/tessdata" "$staging/licenses/tessdata_fast"
curl --fail --silent --show-error --location --max-time 300 \
    --output "$staging/licenses/tessdata_fast/LICENSE" \
    "https://raw.githubusercontent.com/tesseract-ocr/tessdata_fast/$TESSDATA_COMMIT/LICENSE"
"$here/fetch-tessdata.sh" "$staging/tessdata"
rm -rf "$output"
mv "$staging" "$output"
