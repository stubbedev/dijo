#!/usr/bin/env bash
set -euo pipefail

# Builds the statically linked Linux executable without Tika: the program alone, linked with musl,
# tesseract, Leptonica and libheif, needing nothing at run time and no GraalVM to build.

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <output executable>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$here/.." && pwd)
output=$(realpath -m "$1")

case "$(uname -m)" in
    x86_64) triple=x86_64-unknown-linux-musl ;;
    aarch64) triple=aarch64-unknown-linux-musl ;;
    *) echo "No static build for $(uname -m)" >&2; exit 1 ;;
esac

toolchain=$("$here/static-toolchain.sh")
work="$root/target/static"
mkdir -p "$work"

if [ -z "${EXTRACTOUS_OCR_BUNDLE:-}" ]; then
    "$here/ocr-bundle.sh" --linked "$work/ocr-bundle"
    export EXTRACTOUS_OCR_BUNDLE="$work/ocr-bundle"
fi

triple_variable=${triple//-/_}
flags="$toolchain/extractous"
env "CC_$triple_variable=$toolchain/bin/$triple-gcc" "CXX_$triple_variable=$toolchain/bin/$triple-g++" \
    "AR_$triple_variable=$toolchain/bin/$triple-ar" \
    "CARGO_TARGET_${triple_variable^^}_LINKER=$toolchain/bin/$triple-gcc" \
    "CARGO_TARGET_${triple_variable^^}_RUSTFLAGS=-C target-feature=+crt-static" \
    PATH="$toolchain/bin:$PATH" \
    EXTRACTOUS_STATIC_LIBS="$flags/tesseract-libs:$flags/images-libs" \
    EXTRACTOUS_STATIC_LICENSES="$flags/decoder-licenses" \
    cargo build --manifest-path "$root/Cargo.toml" -p extractous --bin extractous --release --locked \
    --target "$triple" --no-default-features --features static
mkdir -p "$(dirname "$output")"
cp "${CARGO_TARGET_DIR:-$root/target}/$triple/release/extractous" "$output"
