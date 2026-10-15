#!/usr/bin/env bash
set -euo pipefail

# The tesseract the builds other than the static executable load into their process: a library of the JNI
# functions LinkedTesseract calls (extractous-core/tika-native/src/static/tesseract.cc), linked
# with tesseract built from its pinned source, patched as the Linux builds patch it
# (ci/tesseract.nix), and with Leptonica and the image libraries vcpkg builds statically at a
# pinned commit (ci/tesseract-jni); on Linux and Windows heif-png (ci/heif-png) to decode HEIC,
# HEIF and AVIF, which macOS decodes with its own sips; and the license files of everything linked
# into them. On Linux it builds against the system's C library, which the glibc release binary
# then needs, as it does.

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <output directory>" >&2
    exit 2
fi
: "${JAVA_HOME:?JAVA_HOME must name the JDK whose JNI headers the tesseract library is built with}"

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
output=$1
# shellcheck source=ci/vcpkg.env
source "$here/vcpkg.env"

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64 | Linux-aarch64)
        if [ "$(uname -m)" = x86_64 ]; then
            triplet=x64-linux
        else
            triplet=arm64-linux
            export VCPKG_FORCE_SYSTEM_BINARIES=1
        fi
        library=libextractous_tesseract.so bootstrap=(./bootstrap-vcpkg.sh) heif=heif-png
        sha512=(sha512sum)
        native() { printf '%s\n' "$1"; }
        ;;
    Darwin-arm64 | Darwin-x86_64)
        if [ "$(uname -m)" = arm64 ]; then triplet=arm64-osx; else triplet=x64-osx; fi
        library=libextractous_tesseract.dylib bootstrap=(./bootstrap-vcpkg.sh) heif=
        sha512=(shasum -a 512)
        native() { printf '%s\n' "$1"; }
        ;;
    MINGW*-x86_64 | MSYS*-x86_64)
        triplet=x64-windows-static library=extractous_tesseract.dll
        bootstrap=(cmd //c bootstrap-vcpkg.bat) heif="heif-png.exe" sha512=(sha512sum)
        native() { cygpath -m "$1"; }
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
toolchain=(-DCMAKE_TOOLCHAIN_FILE="$(native "$root/scripts/buildsystems/vcpkg.cmake")"
    -DVCPKG_TARGET_TRIPLET="$triplet")

"$root/vcpkg" install --triplet "$triplet" --clean-after-build leptonica
archive="$root/tesseract.tar.gz"
curl --fail --silent --show-error --location --max-time 300 --output "$archive" \
    "https://github.com/tesseract-ocr/tesseract/archive/$TESSERACT_VERSION.tar.gz"
if [ "$("${sha512[@]}" "$archive" | cut -d' ' -f1)" != "$TESSERACT_SHA512" ]; then
    echo "tesseract $TESSERACT_VERSION's source archive does not have the pinned SHA-512" >&2
    exit 1
fi
mkdir "$root/tesseract"
tar -xzf "$archive" -C "$root/tesseract" --strip-components 1
patch --directory "$root/tesseract" --strip 1 --input "$here/tesseract-inline-stridemap.patch"
cmake -S "$(native "$here/tesseract-jni")" -B "$(native "$root/tesseract-jni")" "${toolchain[@]}" \
    -DCMAKE_BUILD_TYPE=Release -DTESSERACT_SOURCE="$(native "$root/tesseract")"
cmake --build "$(native "$root/tesseract-jni")" --config Release --target extractous_tesseract

if [ -n "$heif" ]; then
    "$root/vcpkg" install --triplet "$triplet" --clean-after-build "libheif[core,aom]"
    cmake -S "$(native "$here/heif-png")" -B "$(native "$root/heif-png")" "${toolchain[@]}" \
        -DCMAKE_BUILD_TYPE=Release
    cmake --build "$(native "$root/heif-png")" --config Release
fi

installed="$root/installed"
staging="$output.partial"
rm -rf "$staging"
mkdir -p "$staging/licenses/tesseract"
cp "$root/tesseract-jni/out/$library" "$staging/$library"
cp "$root/tesseract/LICENSE" "$staging/licenses/tesseract/LICENSE"
if [ -n "$heif" ]; then
    cp "$root/heif-png/out/$heif" "$staging/$heif"
fi
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
