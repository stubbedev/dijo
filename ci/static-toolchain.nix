# What ci/static-native.nix builds the static executable with, for the Linux of `system`: the
# musl package set, tesseract with Leptonica and the image libraries it reads with (whose licenses
# ci/ocr-static.nix collects), libheif (ci/image-decoders.nix), the library flag files
# DIJO_STATIC_LIBS lists and the licenses of the libraries linked beyond tesseract's.
{ nixpkgs, system }:
let
  pkgs = nixpkgs.legacyPackages.${system};
  lib = pkgs.lib;
  cross =
    {
      x86_64-linux = pkgs.pkgsCross.musl64;
      aarch64-linux = pkgs.pkgsCross.aarch64-multiplatform-musl;
    }
    .${system};
  static = import ./static-packages.nix pkgs;
  tesseract = import ./tesseract.nix static;
  decoders = import ./image-decoders.nix static;
  decoderLibraries = [
    decoders.libheif
    static.libde265
    static.dav1d
  ];
  pkgConfigPath = lib.makeSearchPathOutput "dev" "lib/pkgconfig";
  decoderLicenses = pkgs.runCommand "dijo-decoder-licenses" { } (
    lib.concatMapStrings (import ./licenses.nix "$out/dijo/decoder-licenses") decoderLibraries
  );
  decoderFlags =
    pkgs.runCommand "dijo-image-decoder-flags" { nativeBuildInputs = [ pkgs.pkg-config ]; }
      ''
        export PKG_CONFIG_PATH=${pkgConfigPath decoderLibraries}
        mkdir -p $out/dijo
        pkg-config --static --libs libheif | tr -s ' ' '\n' | sed '/^$/d' \
          > $out/dijo/images-libs
      '';
  tesseractFlags =
    pkgs.runCommand "dijo-tesseract-flags" { nativeBuildInputs = [ pkgs.pkg-config ]; }
      ''
        export PKG_CONFIG_PATH=${tesseract}/lib/pkgconfig:${static.leptonica}/lib/pkgconfig
        mkdir -p $out/dijo
        { pkg-config --static --libs tesseract; echo -L${static.giflib}/lib -lm; } \
          | tr -s ' ' '\n' | sed '/^$/d' > $out/dijo/tesseract-libs
      '';
in
{
  inherit cross tesseract;
  staticLibs = [
    "${tesseractFlags}/dijo/tesseract-libs"
    "${decoderFlags}/dijo/images-libs"
  ];
  decoderLicenses = "${decoderLicenses}/dijo/decoder-licenses";
}
