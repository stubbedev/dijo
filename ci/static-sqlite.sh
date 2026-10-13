#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 4 ]; then
    echo "usage: $0 <sqlite-jdbc version> <musl gcc> <slf4j-api jar> <output directory>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
version=$1
cc=$2
slf4j=$3
output=$(realpath -m "$4")
: "${GRAALVM_HOME:?GRAALVM_HOME must point to the GraalVM that builds the image}"

digest() {
    local sum
    sum=$(sha256sum < "$1")
    echo "${sum%% *}"
}

fetch() {
    local name=$1 url sha256
    read -r url sha256 < <(awk -F'\t' -v name="$name" '$1 == name { print $2, $3 }' "$here/static-sources.tsv")
    if [ -z "${url:-}" ]; then
        echo "ci/static-sources.tsv pins no $name" >&2
        exit 1
    fi
    local file="$output/sources/$name"
    if [ ! -f "$file" ] || [ "$(digest "$file")" != "$sha256" ]; then
        mkdir -p "$output/sources"
        curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location --retry 5 --max-time 600 \
            --output "$file.part" "$url"
        if [ "$(digest "$file.part")" != "$sha256" ]; then
            rm -f "$file.part"
            echo "$url does not have the pinned SHA-256 $sha256" >&2
            exit 1
        fi
        mv "$file.part" "$file"
    fi
    echo "$file"
}

stamp="$version $cc"
if [ -f "$output/libsqlitejdbc.a" ] && [ "$(cat "$output/built" 2>/dev/null)" = "$stamp" ]; then
    exit 0
fi
rm -f "$output/built"

sqlite_version=${version%.*}
jdbc=$(fetch "sqlite-jdbc-$version.tar.gz")
sqlite=$(fetch "sqlite-$sqlite_version-src.zip")

source_dir="$output/sqlite-jdbc-$version"
rm -rf "$source_dir"
tar -xzf "$jdbc" -C "$output"
mkdir -p "$source_dir/target"
cp "$sqlite" "$source_dir/target/sqlite-$sqlite_version-src.zip"

objects="target/sqlite-$sqlite_version-Linux-Musl-$(uname -m)"
make -C "$source_dir" OS_NAME=Linux-Musl OS_ARCH="$(uname -m)" \
    CC="$cc -Wno-error=implicit-function-declaration" JAVA_HOME="$GRAALVM_HOME" \
    JAVA_CLASSPATH="$slf4j" "$objects/libsqlitejdbc.so"

objcopy="${cc%gcc}objcopy"
"$objcopy" --redefine-sym JNI_OnLoad=JNI_OnLoad_sqlitejdbc \
    --redefine-sym JNI_OnUnload=JNI_OnUnload_sqlitejdbc "$source_dir/$objects/NativeDB.o"
rm -f "$output/libsqlitejdbc.a"
"${cc%gcc}ar" rcs "$output/libsqlitejdbc.a" "$source_dir/$objects/NativeDB.o" "$source_dir/$objects/sqlite3.o"
"${cc%gcc}nm" --defined-only --extern-only --format=just-symbols "$output/libsqlitejdbc.a" \
    | grep -E '^(Java_|JNI_On)' | sort > "$output/libsqlitejdbc.a.symbols"
echo "$stamp" > "$output/built"
