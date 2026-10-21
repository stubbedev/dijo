# The nixpkgs flake.lock pins, for the scripts that build with nix outside the flake, so they share
# the flake's and the dev shell's store paths and binary cache hits.
let
  lock = builtins.fromJSON (builtins.readFile ../flake.lock);
  locked = lock.nodes.${lock.nodes.${lock.root}.inputs.nixpkgs}.locked;
in
builtins.getFlake "github:${locked.owner}/${locked.repo}/${locked.rev}?narHash=${locked.narHash}"
