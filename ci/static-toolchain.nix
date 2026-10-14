# What ci/static-image.sh links the static executable with, for the Linux of `system`: the musl
# C compiler and binutils, the musl Rust toolchain, the static C libraries the JDK's AWT archives
# need (FreeType and what it reads fonts with), tesseract with Leptonica and the image libraries
# it reads with (the ones ci/tesseract-static.nix bundles as a program), libheif and DjVuLibre
# (ci/image-decoders.nix), with the compiler and linker flags for them in extractous/, the
# licenses of the libraries linked beyond tesseract's in extractous/licenses, and what
# sqlite-jdbc's Makefile builds SQLite with (ci/static-sqlite.sh).
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
  static = pkgs.pkgsStatic;
  tesseract = import ./tesseract.nix static;
  decoders = import ./image-decoders.nix static;
  awtLibraries = [
    cross.pkgsStatic.zlib
    cross.pkgsStatic.freetype
    cross.pkgsStatic.libpng
    cross.pkgsStatic.bzip2
    cross.pkgsStatic.brotli
  ];
  decoderLibraries = [
    decoders.libheif
    static.libde265
    static.dav1d
    decoders.djvulibre
  ];
  pkgConfigPath = lib.makeSearchPathOutput "dev" "lib/pkgconfig";
  licenses =
    pkgs.runCommand "extractous-static-licenses" { }
      (lib.concatMapStrings (import ./licenses.nix "$out/extractous/licenses") (
        awtLibraries ++ decoderLibraries
      ));
  decoderFlags =
    pkgs.runCommand "extractous-image-decoder-flags" { nativeBuildInputs = [ pkgs.pkg-config ]; }
      ''
        export PKG_CONFIG_PATH=${pkgConfigPath (decoderLibraries ++ [ static.libjpeg ])}
        mkdir -p $out/extractous
        pkg-config --static --cflags libheif ddjvuapi | tr -s ' ' '\n' | sed '/^$/d' \
          > $out/extractous/images-cflags
        pkg-config --static --libs libheif ddjvuapi | tr -s ' ' '\n' | sed '/^$/d' \
          > $out/extractous/images-libs
      '';
  tesseractFlags =
    pkgs.runCommand "extractous-tesseract-flags" { nativeBuildInputs = [ pkgs.pkg-config ]; }
      ''
        export PKG_CONFIG_PATH=${tesseract}/lib/pkgconfig:${static.leptonica}/lib/pkgconfig
        mkdir -p $out/extractous
        pkg-config --static --cflags tesseract lept | tr -s ' ' '\n' | sed '/^$/d' \
          > $out/extractous/tesseract-cflags
        { pkg-config --static --libs tesseract; echo -L${static.giflib}/lib -lm; } \
          | tr -s ' ' '\n' | sed '/^$/d' > $out/extractous/tesseract-libs
      '';
in
pkgs.symlinkJoin {
  name = "extractous-static-toolchain";
  paths = map lib.getLib awtLibraries ++ [
    cross.stdenv.cc
    cross.buildPackages.cargo
    cross.buildPackages.rustc
    tesseractFlags
    decoderFlags
    licenses
    pkgs.gnumake
    pkgs.gcc
    pkgs.tcl
    pkgs.perl
    pkgs.unzip
    pkgs.zip
  ];
}
