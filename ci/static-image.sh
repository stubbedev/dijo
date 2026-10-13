#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <output executable>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$here/.." && pwd)
output=$(realpath -m "$1")
: "${GRAALVM_HOME:?GRAALVM_HOME must point to the GraalVM that builds the image}"

case "$(uname -m)" in
    x86_64) triple=x86_64-unknown-linux-musl platform=linux-amd64 ;;
    aarch64) triple=aarch64-unknown-linux-musl platform=linux-aarch64 ;;
    *) echo "No static build for $(uname -m)" >&2; exit 1 ;;
esac

rev=$(nix eval --raw --impure --expr \
    "(builtins.fromJSON (builtins.readFile $root/devenv.lock)).nodes.nixpkgs.locked.rev")
toolchain=$(nix build --no-link --print-out-paths --impure --expr \
    "import $here/static-toolchain.nix { nixpkgs = builtins.getFlake \"github:cachix/devenv-nixpkgs/$rev\"; system = builtins.currentSystem; }")
cc="$toolchain/bin/$triple-gcc"

work="$root/target/static-image"
mkdir -p "$work/feature" "$work/clibraries"

if [ -z "${EXTRACTOUS_OCR_BUNDLE:-}" ]; then
    "$here/ocr-bundle.sh" "$work/ocr-bundle"
    export EXTRACTOUS_OCR_BUNDLE="$work/ocr-bundle"
fi

triple_variable=${triple//-/_}
env "CC_$triple_variable=$cc" "AR_$triple_variable=$toolchain/bin/$triple-ar" \
    PATH="$toolchain/bin:$PATH" \
    cargo rustc --manifest-path "$root/Cargo.toml" -p extractous --lib --release --locked \
    --target "$triple" --features static-image --crate-type staticlib
program="$root/target/$triple/release/libextractous.a"

cp "$GRAALVM_HOME/lib/svm/clibraries/$platform/musl/libjvm.a" "$work/clibraries/libjvm.a"
chmod u+w "$work/clibraries/libjvm.a"
"$toolchain/bin/$triple-objcopy" --weaken-symbol=JVM_IsStaticallyLinked "$work/clibraries/libjvm.a"
static_sources="$root/extractous-core/tika-native/src/static"
objects=()
for source in "$static_sources"/*.c; do
    object="$work/$(basename "$source" .c).o"
    "$cc" -c -O2 -fPIC -I"$GRAALVM_HOME/include" -I"$GRAALVM_HOME/include/linux" -o "$object" "$source"
    objects+=("$object")
done
mapfile -t tesseract_cflags < "$toolchain/extractous/tesseract-cflags"
mapfile -t tesseract_libs < "$toolchain/extractous/tesseract-libs"
"$toolchain/bin/$triple-g++" -c -O2 -fPIC -std=c++17 "${tesseract_cflags[@]}" \
    -I"$GRAALVM_HOME/include" -I"$GRAALVM_HOME/include/linux" \
    -o "$work/tesseract.o" "$static_sources/tesseract.cc"
objects+=("$work/tesseract.o")

(cd "$root/extractous-core/tika-native" && ./gradlew -q nativeImageArguments) > "$work/arguments"
mapfile -t arguments < "$work/arguments"
classpath_index=-1
for index in "${!arguments[@]}"; do
    if [ "${arguments[$index]}" = "-cp" ]; then
        classpath_index=$((index + 1))
    fi
done
if [ "$classpath_index" -lt 0 ]; then
    echo "gradle printed no classpath" >&2
    exit 1
fi
classpath=${arguments[classpath_index]}
arguments[classpath_index]="$classpath:$work/feature"

mapfile -t static_java < <(find "$static_sources/java" -name '*.java' | sort)
"$GRAALVM_HOME/bin/javac" --add-exports org.graalvm.nativeimage.builder/com.oracle.svm.core.jdk=ALL-UNNAMED \
    --add-exports org.graalvm.nativeimage.builder/com.oracle.svm.hosted.c=ALL-UNNAMED \
    -cp "$GRAALVM_HOME/lib/svm/builder/svm.jar:$classpath" -d "$work/feature" "${static_java[@]}"

jar_in_classpath() {
    tr ':' '\n' <<< "$classpath" | grep -E "/$1-[0-9][^/]*\.jar$" | grep -v sources
}
sqlite_jar=$(jar_in_classpath sqlite-jdbc)
sqlite_version=$(basename "$sqlite_jar" .jar)
sqlite_version=${sqlite_version#sqlite-jdbc-}
PATH="$toolchain/bin:$PATH" "$here/static-sqlite.sh" "$sqlite_version" "$cc" \
    "$(jar_in_classpath slf4j-api)" "$work/sqlite"

native_image="$GRAALVM_HOME/bin/native-image"
if [ -x "$GRAALVM_HOME/bin/.native-image-wrapped" ]; then
    native_image="$GRAALVM_HOME/bin/.native-image-wrapped"
fi

linked=()
for library in "${objects[@]}" "$program" "${tesseract_libs[@]}" -lfreetype -lpng16 -lbz2 \
    -lbrotlidec -lbrotlicommon -lz -lstdc++; do
    linked+=("-H:NativeLinkerOption=$library")
done

cd "$root/extractous-core/tika-native"
env -u LIBRARY_PATH -u NIX_LDFLAGS -u NIX_CFLAGS_COMPILE "$native_image" \
    "${arguments[@]}" \
    --static --libc=musl --native-compiler-path="$cc" \
    -H:CLibraryPath="$work/clibraries" -H:CLibraryPath="$work/sqlite" -H:CLibraryPath="$toolchain/lib" \
    --add-exports=org.graalvm.nativeimage.builder/com.oracle.svm.core.jdk=ALL-UNNAMED \
    --add-exports=org.graalvm.nativeimage.builder/com.oracle.svm.hosted.c=ALL-UNNAMED \
    --features=ai.yobix.StaticJniLibrariesFeature \
    -H:-ParseRuntimeOptions \
    "${linked[@]}" \
    -H:Class=ai.yobix.StaticImage \
    -o "$output"
