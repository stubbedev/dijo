{ pkgs, ... }:

let
  graalvm = pkgs.graalvmPackages.graalvm-ce;
in
{
  packages = [
    pkgs.just
    pkgs.cargo-nextest
    pkgs.pkg-config
    pkgs.openssl
    (pkgs.tesseract.override {
      enableLanguages = [
        "eng"
        "deu"
        "ara"
      ];
    })
    pkgs.git
  ];

  languages.rust.enable = true;

  languages.java = {
    enable = true;
    jdk.package = graalvm;
  };

  env.GRAALVM_HOME = graalvm;
}
