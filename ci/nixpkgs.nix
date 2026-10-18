# The nixpkgs flake devenv.lock pins, for every Nix build outside devenv: the flake's outputs and
# the scripts that build with nix, so they share devenv's store paths and binary cache hits.
let
  locked = (builtins.fromJSON (builtins.readFile ../devenv.lock)).nodes.nixpkgs.locked;
in
builtins.getFlake "github:${locked.owner}/${locked.repo}/${locked.rev}?narHash=${locked.narHash}"
