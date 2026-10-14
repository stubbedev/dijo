# A statically linked tesseract (musl), without the training tools, heif-png (ci/heif-png), which
# decodes HEIC, HEIF and AVIF images with libheif for it, and the license files of everything
# linked into them: tesseract, libheif, the static libraries they link, the C library and GCC's
# runtime.
{ nixpkgs, system }:
let
  pkgs = nixpkgs.legacyPackages.${system};
  lib = pkgs.lib;
  static = pkgs.pkgsStatic;
  tesseract = import ./tesseract.nix static;
  decoders = import ./image-decoders.nix static;
  heifPng = static.stdenv.mkDerivation {
    name = "heif-png";
    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ./heif-png/main.cc
        ../extractous-core/tika-native/src/static/heif.cc
        ../extractous-core/tika-native/src/static/heif.h
      ];
    };
    nativeBuildInputs = [ static.buildPackages.pkg-config ];
    buildInputs = [
      decoders.libheif
      static.leptonica
    ];
    buildPhase = ''
      $CXX -O2 -std=c++17 -Iextractous-core/tika-native/src/static ci/heif-png/main.cc \
        extractous-core/tika-native/src/static/heif.cc \
        $($PKG_CONFIG --static --cflags --libs libheif lept) -static -o heif-png
    '';
    installPhase = ''
      install -D heif-png $out/bin/heif-png
    '';
  };
  inputsOf =
    p:
    lib.filter (input: input != null && input ? src) (
      (p.buildInputs or [ ]) ++ (p.propagatedBuildInputs or [ ])
    );
  closure = lib.genericClosure {
    startSet = map (p: {
      key = p.name;
      package = p;
    }) (inputsOf tesseract ++ inputsOf heifPng);
    operator =
      item:
      map (p: {
        key = p.name;
        package = p;
      }) (inputsOf item.package);
  };
  licenses = import ./licenses.nix "$out/licenses";
in
pkgs.runCommand "tesseract-static-bundle" { } (
  ''
    mkdir -p $out/licenses
    cp ${tesseract}/bin/tesseract $out/tesseract
    cp ${heifPng}/bin/heif-png $out/heif-png
  ''
  + lib.concatMapStrings licenses [
    tesseract
    static.stdenv.cc.libc
    static.stdenv.cc.cc
  ]
  + lib.concatMapStrings (p: ''
    if [ -n "$(find ${
      lib.concatMapStringsSep " " (output: p.${output}) (p.outputs or [ "out" ])
    } -path '*/lib/*.a' -print -quit)" ]; then
      ${licenses p}
    fi
  '') (map (item: item.package) closure)
)
