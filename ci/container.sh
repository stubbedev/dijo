#!/usr/bin/env bash
set -euo pipefail

# Builds the container image extractous:local around a statically linked executable
# (ci/container.nix) and prints the path of its archive, for `docker load`.

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <static executable>" >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
EXTRACTOUS_EXECUTABLE=$(realpath "$1") nix build --no-link --print-out-paths --impure --expr \
    "import $here/container.nix {
       pkgs = (import $here/nixpkgs.nix).legacyPackages.\${builtins.currentSystem};
       executable = builtins.path { path = /. + builtins.getEnv \"EXTRACTOUS_EXECUTABLE\"; name = \"extractous\"; };
     }"
