#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <linux-x64|linux-aarch64|macos-aarch64|windows-x64> <directory>" >&2
    exit 2
fi
platform="$1"
directory="$2"

# shellcheck source=ci/graalvm.env
source "$(dirname "${BASH_SOURCE[0]}")/graalvm.env"

checksum_variable="GRAALVM_SHA256_${platform//-/_}"
checksum="${!checksum_variable:-}"
if [ -z "$checksum" ]; then
    echo "no pinned GraalVM checksum for platform $platform" >&2
    exit 2
fi
case "$platform" in
    windows-*) extension=zip ;;
    *) extension=tar.gz ;;
esac
asset="graalvm-community-jdk-${GRAALVM_BUILD}_${platform}_bin.${extension}"
archive="$directory/$asset"

mkdir -p "$directory"
curl --fail --silent --show-error --location --output "$archive" \
    "https://github.com/graalvm/graalvm-ce-builds/releases/download/$GRAALVM_TAG/$asset"
if sha256sum --version > /dev/null 2>&1; then
    echo "$checksum  $archive" | sha256sum --check --strict >&2
else
    echo "$checksum  $archive" | shasum --algorithm 256 --check --strict >&2
fi
case "$extension" in
    zip) unzip -q "$archive" -d "$directory" ;;
    *) tar -xzf "$archive" -C "$directory" ;;
esac
rm "$archive"

for bin in "$directory"/*/bin "$directory"/*/Contents/Home/bin; do
    if [ -e "$bin/native-image" ] || [ -e "$bin/native-image.cmd" ]; then
        dirname "$bin"
        exit 0
    fi
done
echo "$asset contains no native-image" >&2
exit 1
