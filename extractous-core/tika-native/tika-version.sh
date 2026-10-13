#!/usr/bin/env bash
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
version=$(sed -n 's/^def tikaVersion = "\(.*\)"$/\1/p' "$here/build.gradle")
if [ -z "$version" ]; then
    echo "$here/build.gradle defines no tikaVersion" >&2
    exit 1
fi
echo "$version"
