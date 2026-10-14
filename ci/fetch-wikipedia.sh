#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <language-groups.tsv> <directory>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

# shellcheck source=ci/fetch-pinned.sh
source "$here/fetch-pinned.sh"
sources=$1
dir=$2
revision=$(sed -n 's/^WIKIPEDIA_REVISION = "\([0-9a-f]*\)"$/\1/p' "$(dirname "$sources")/language-groups.py")
if [ -z "$revision" ]; then
    echo "language-groups.py names no WIKIPEDIA_REVISION" >&2
    exit 1
fi
base="https://huggingface.co/datasets/wikimedia/wikipedia/resolve/$revision"

while IFS=$'\t' read -r _ _ _ shard sha256 _; do
    fetch_pinned "$base/$shard" "$sha256" "$dir/$shard"
done < <(grep -v '^#' "$sources" | sort -u -t $'\t' -k4,4)
