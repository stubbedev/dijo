#!/usr/bin/env bash
set -euo pipefail

# Builds what the static executables link with (ci/static-toolchain.nix) for this machine and
# prints its store path.

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
rev=$(nix eval --raw --impure --expr \
    "(builtins.fromJSON (builtins.readFile $here/../devenv.lock)).nodes.nixpkgs.locked.rev")
nix build --no-link --print-out-paths --impure --expr \
    "import $here/static-toolchain.nix { nixpkgs = builtins.getFlake \"github:cachix/devenv-nixpkgs/$rev\"; system = builtins.currentSystem; }"
