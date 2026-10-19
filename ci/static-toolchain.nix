# What ci/static-native.nix builds the static executable with, for the Linux of `system`: the
# musl package set, tesseract with Leptonica and the image libraries it reads with (whose licenses
# ci/ocr-static.nix collects), libheif (ci/image-decoders.nix), the library flag files
# EXTRACTOUS_STATIC_LIBS lists and the licenses of the libraries linked beyond tesseract's.
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
  decoderLicenses = pkgs.runCommand "extractous-decoder-licenses" { } (
    lib.concatMapStrings (import ./licenses.nix "$out/extractous/decoder-licenses") decoderLibraries
  );
  decoderFlags =
    pkgs.runCommand "extractous-image-decoder-flags" { nativeBuildInputs = [ pkgs.pkg-config ]; }
      ''
        export PKG_CONFIG_PATH=${pkgConfigPath decoderLibraries}
        mkdir -p $out/extractous
        pkg-config --static --libs libheif | tr -s ' ' '\n' | sed '/^$/d' \
          > $out/extractous/images-libs
      '';
  tesseractFlags =
    pkgs.runCommand "extractous-tesseract-flags" { nativeBuildInputs = [ pkgs.pkg-config ]; }
      ''
        export PKG_CONFIG_PATH=${tesseract}/lib/pkgconfig:${static.leptonica}/lib/pkgconfig
        mkdir -p $out/extractous
        { pkg-config --static --libs tesseract; echo -L${static.giflib}/lib -lm; } \
          | tr -s ' ' '\n' | sed '/^$/d' > $out/extractous/tesseract-libs
      '';
in
{
  inherit cross tesseract;
  staticLibs = [
    "${tesseractFlags}/extractous/tesseract-libs"
    "${decoderFlags}/extractous/images-libs"
  ];
  decoderLicenses = "${decoderLicenses}/extractous/decoder-licenses";
}
