{ pkgs, lib, ... }:

let
  graalvm = pkgs.graalvmPackages.graalvm-ce;
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
    (pkgs.tesseract.override { enableLanguages = [ "eng" ]; })
    pkgs.libheif.bin
    pkgs.git
    pkgs.sccache
  ] ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.mold-wrapped ];

  languages.rust.enable = true;

  languages.java = {
    enable = true;
    jdk.package = graalvm;
  };

  env = {
    GRAALVM_HOME = graalvm;
    OMP_THREAD_LIMIT = "1";
    TESSDATA_PREFIX = tessdata;
    RUSTC_WRAPPER = "sccache";
  } // lib.optionalAttrs pkgs.stdenv.isLinux {
    "CARGO_TARGET_${hostTarget}_LINKER" = lib.mkForce (lib.getExe pkgs.clang);
    "CARGO_TARGET_${hostTarget}_RUSTFLAGS" = "-C link-arg=-fuse-ld=mold";
  };
}
