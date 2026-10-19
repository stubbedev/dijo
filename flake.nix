{
  description = "dijo's Nix-built release artifacts: the static Linux executable and its container image";

  inputs.crane.url = "github:ipetkov/crane/v0.24.0";

  outputs =
    { crane, ... }:
    let
      nixpkgs = import ./ci/nixpkgs.nix;
      inherit (nixpkgs) lib;
    in
    {
      packages = lib.genAttrs [ "x86_64-linux" "aarch64-linux" ] (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          static = import ./ci/static-native.nix { inherit nixpkgs crane system; };
        in
        {
          default = static.executable;
          static = static.executable;
          static-dependencies = static.dependencies;
          ocr-bundle-linked = import ./ci/ocr-static.nix { inherit nixpkgs system; };
          container = import ./ci/container.nix {
            inherit pkgs;
            executable = lib.getExe static.executable;
          };
        }
      );
    };
}
