# What ci/static-image.sh links the static executable with, for the Linux of `system`: the musl
# C compiler and binutils, the musl Rust toolchain, the static C libraries the JDK's AWT archives
# need (FreeType and what it reads fonts with), and what sqlite-jdbc's Makefile builds SQLite
# with (ci/static-sqlite.sh).
{ nixpkgs, system }:
let
  pkgs = nixpkgs.legacyPackages.${system};
  cross = {
    x86_64-linux = pkgs.pkgsCross.musl64;
    aarch64-linux = pkgs.pkgsCross.aarch64-multiplatform-musl;
  }.${system};
in
pkgs.symlinkJoin {
  name = "extractous-static-toolchain";
  paths = [
    cross.stdenv.cc
    cross.buildPackages.cargo
    cross.buildPackages.rustc
    cross.pkgsStatic.zlib
    cross.pkgsStatic.freetype
    cross.pkgsStatic.libpng
    cross.pkgsStatic.bzip2.out
    cross.pkgsStatic.brotli.lib
    pkgs.gnumake
    pkgs.gcc
    pkgs.tcl
    pkgs.perl
    pkgs.unzip
    pkgs.zip
  ];
}
