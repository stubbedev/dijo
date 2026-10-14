# shellcheck shell=bash

sha256_of() {
    local sum
    if command -v sha256sum > /dev/null; then
        sum=$(sha256sum < "$1")
    else
        sum=$(shasum -a 256 < "$1")
    fi
    echo "${sum%% *}"
}

fetch_pinned() {
    local url=$1 sha256=$2 file=$3 actual
    if [ -f "$file" ] && [ "$(sha256_of "$file")" = "$sha256" ]; then
        return
    fi
    mkdir -p "$(dirname "$file")"
    curl --proto '=https' --tlsv1.2 --fail --silent --show-error --location --retry 5 --max-time 600 \
        --output "$file.part" "$url"
    actual=$(sha256_of "$file.part")
    if [ "$actual" != "$sha256" ]; then
        rm -f "$file.part"
        echo "$url has the SHA-256 $actual, not the pinned $sha256" >&2
        exit 1
    fi
    mv "$file.part" "$file"
}
