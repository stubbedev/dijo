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

rev=$(nix eval --raw --impure --expr \
    "(builtins.fromJSON (builtins.readFile $here/../devenv.lock)).nodes.nixpkgs.locked.rev")
bundle=$(nix build --no-link --print-out-paths --impure --expr \
    "import $here/tesseract-static.nix { nixpkgs = builtins.getFlake \"github:cachix/devenv-nixpkgs/$rev\"; system = builtins.currentSystem; }")

staging="$output.partial"
rm -rf "$staging"
mkdir -p "$staging/tessdata" "$staging/licenses/tessdata_fast"
install -m 755 "$bundle/tesseract" "$staging/tesseract"
cp -r "$bundle/licenses/." "$staging/licenses/"
chmod -R u+w "$staging/licenses"
tessdata="https://raw.githubusercontent.com/tesseract-ocr/tessdata_fast/$TESSDATA_COMMIT"
curl --fail --silent --show-error --location --max-time 300 \
    --output "$staging/licenses/tessdata_fast/LICENSE" "$tessdata/LICENSE"
while IFS=$'\t' read -r language sha256; do
    model="$staging/tessdata/$language.traineddata"
    curl --fail --silent --show-error --location --max-time 300 --output "$model" \
        "$tessdata/$language.traineddata"
    echo "$sha256  $model" | sha256sum --check --quiet
done < <(grep -v '^#' "$here/tessdata.tsv")
rm -rf "$output"
mv "$staging" "$output"
