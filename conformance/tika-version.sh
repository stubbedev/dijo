#!/usr/bin/env bash
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
version=$(<"$here/tika-version")
if [ -z "$version" ]; then
    echo "$here/tika-version names no Tika version" >&2
    exit 1
fi
echo "$version"
