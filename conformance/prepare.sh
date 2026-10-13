#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <work directory>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
dir=$1
version=$("$here/../extractous-core/tika-native/tika-version.sh")
pins=$(awk -v version="$version" '$1 == version { print $2, $3 }' "$here/pins.tsv")
if [ -z "$pins" ]; then
    echo "conformance/pins.tsv pins no corpus commit and tika-app checksum for Tika $version" >&2
    exit 1
fi
read -r commit _ <<< "$pins"
mkdir -p "$dir"

repository="$dir/tika-$version"
if [ ! -d "$repository/.git" ]; then
    rm -rf "$repository.partial"
    git init --quiet "$repository.partial"
    git -C "$repository.partial" remote add origin https://github.com/apache/tika.git
    git -C "$repository.partial" sparse-checkout set --no-cone '/tika-parsers/**/src/test/resources/test-documents/**'
    git -C "$repository.partial" fetch --quiet --depth 1 --filter=blob:none origin "$commit"
    git -C "$repository.partial" checkout --quiet FETCH_HEAD
    mv "$repository.partial" "$repository"
fi
if [ "$(git -C "$repository" rev-parse HEAD)" != "$commit" ]; then
    echo "$repository is not at the pinned commit $commit" >&2
    exit 1
fi

jvm="$dir/jvm-$version"
if [ ! -d "$jvm" ]; then
    rm -rf "$jvm.partial"
    "$here/tika-app.sh" "$dir" "$repository/tika-parsers" "$jvm.partial" \
        --config="$here/tika-config.json" > "$dir/jvm-$version.log" 2>&1
    mv "$jvm.partial" "$jvm"
fi

echo "export CONFORMANCE_CORPUS='$repository/tika-parsers'"
echo "export CONFORMANCE_JVM='$jvm'"
