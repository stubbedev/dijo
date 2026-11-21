#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <work directory> <output tsv>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
root=$(cd "$here/.." && pwd)
dir=$1
output=$2
version=$("$here/tika-version.sh")
app=$("$here/fetch-tika.sh" "$dir" app)
eval "$("$here/prepare.sh" "$dir")"

java -Dtika.custom-mimetypes="$root/dijo-mime/models/custom-mimetypes.xml" \
    -cp "$app/tika-app-$version.jar:$app/lib/*" "$here/MimeReference.java" "$output.partial" \
    "documents=$root/test_files/documents" \
    "quality=$root/quality/corpus" \
    "tika=$CONFORMANCE_CORPUS"
mv "$output.partial" "$output"
