#!/usr/bin/env bash
set -euo pipefail

# Builds what the static executables link with (ci/static-toolchain.nix) for this machine and
# prints its store path.

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
nix build --no-link --print-out-paths --impure --expr \
    "import $here/static-toolchain.nix { nixpkgs = import $here/nixpkgs.nix; system = builtins.currentSystem; }"
