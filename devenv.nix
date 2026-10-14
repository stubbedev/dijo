{ pkgs, ... }:

let
  graalvm = pkgs.graalvmPackages.graalvm-ce;
  tessdata = import ./ci/tessdata.nix { inherit pkgs; };
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
  ];

  languages.rust.enable = true;

  languages.java = {
    enable = true;
    jdk.package = graalvm;
  };

  env.GRAALVM_HOME = graalvm;
  env.OMP_THREAD_LIMIT = "1";
  env.TESSDATA_PREFIX = tessdata;
}
