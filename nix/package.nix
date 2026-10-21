{ inputs, ... }:
{
  perSystem =
    {
      lib,
      pkgs,
      system,
      ...
    }:
    {
      packages = lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux (
        let
          static = import ../ci/static-native.nix {
            inherit (inputs) nixpkgs crane;
            inherit system;
          };
        in
        {
          default = static.executable;
          static = static.executable;
          static-dependencies = static.dependencies;
          ocr-bundle-linked = import ../ci/ocr-static.nix {
            inherit (inputs) nixpkgs;
            inherit system;
          };
          container = import ../ci/container.nix {
            inherit pkgs;
            executable = lib.getExe static.executable;
          };
        }
      );
    };
}
