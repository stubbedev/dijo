{ pkgs, lib, ... }:

let
  tessdata = import ./ci/tessdata.nix { inherit pkgs; };
  hostTarget = pkgs.stdenv.hostPlatform.rust.cargoEnvVarTarget;
in
{
  packages = [
    pkgs.just
    pkgs.cargo-nextest
    pkgs.cargo-about
    pkgs.cargo-deny
    pkgs.cargo-cyclonedx
    pkgs.cargo-fuzz
    pkgs.pkg-config
    pkgs.openssl
    (pkgs.tesseract.override {
      enableLanguages = [ "eng" ];
      tesseractBase = pkgs.tesseract5.passthru.tesseractBase.overrideAttrs (old: {
        patches = (old.patches or [ ]) ++ import ./ci/tesseract-patches.nix;
      });
    })
    pkgs.libheif.bin
    pkgs.git
    pkgs.jre_headless
  ] ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.mold-wrapped ];

  languages.rust.enable = true;

  env = {
    OMP_THREAD_LIMIT = "1";
    TESSDATA_PREFIX = tessdata;
  } // lib.optionalAttrs pkgs.stdenv.isLinux {
    "CARGO_TARGET_${hostTarget}_LINKER" = lib.mkForce (lib.getExe pkgs.clang);
    "CARGO_TARGET_${hostTarget}_RUSTFLAGS" = "-C link-arg=-fuse-ld=mold";
  };
}
