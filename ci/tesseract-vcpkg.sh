#!/usr/bin/env bash
set -euo pipefail

# A tesseract executable that links its libraries statically, built with vcpkg at a pinned
# commit for macOS and Windows, and the license files of every library linked into it: the
# layout tesseract-static.nix builds for Linux.

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <output directory>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
output=$1
# shellcheck source=ci/vcpkg.env
source "$here/vcpkg.env"

case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) triplet=arm64-osx executable=tesseract bootstrap=(./bootstrap-vcpkg.sh) ;;
    Darwin-x86_64) triplet=x64-osx executable=tesseract bootstrap=(./bootstrap-vcpkg.sh) ;;
    MINGW*-x86_64 | MSYS*-x86_64)
        triplet=x64-windows-static executable=tesseract.exe bootstrap=(cmd //c bootstrap-vcpkg.bat)
        ;;
    *)
        echo "No vcpkg triplet for $(uname -s) on $(uname -m)" >&2
        exit 1
        ;;
esac

root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
git -C "$root" init --quiet
git -C "$root" fetch --quiet --depth 1 https://github.com/microsoft/vcpkg.git "$VCPKG_COMMIT"
git -C "$root" checkout --quiet FETCH_HEAD
(cd "$root" && "${bootstrap[@]}" -disableMetrics)
"$root/vcpkg" install --triplet "$triplet" --clean-after-build tesseract

installed="$root/installed"
staging="$output.partial"
rm -rf "$staging"
mkdir -p "$staging/licenses"
cp "$installed/$triplet/tools/tesseract/$executable" "$staging/$executable"
for list in "$installed/vcpkg/info/"*"_$triplet.list"; do
    if grep -Eq "^$triplet/lib/[^/]+\.(a|lib)$" "$list"; then
        port=$(basename "$list")
        port=${port%%_*}
        mkdir -p "$staging/licenses/$port"
        cp "$installed/$triplet/share/$port/copyright" "$staging/licenses/$port/copyright"
    fi
done
rm -rf "$output"
mv "$staging" "$output"
