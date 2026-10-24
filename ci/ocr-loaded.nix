# The OCR bundle the development shell's builds embed (DIJO_OCR_BUNDLE), as ci/ocr-vcpkg.sh
# assembles it for the release builds that load tesseract: the library of tesseract's C API
# (ci/tesseract-library) built from tesseract's source patched as every build patches it, heif-png
# (dijo-native/heif-png) linked statically where the platform needs it, the language data
# (ci/tessdata.nix), and the licenses of what they hold.
{ pkgs }:
let
  inherit (pkgs) lib;
  heifDecoded = !pkgs.stdenv.hostPlatform.isDarwin;
  static = import ./static-packages.nix pkgs;
  decoders = import ./image-decoders.nix static;
  tessdata = import ./tessdata.nix { inherit pkgs; };
  licenses = import ./licenses.nix "$out/licenses";
  tesseract = pkgs.tesseract5.passthru.tesseractBase;
  source = pkgs.applyPatches {
    name = "tesseract-${tesseract.version}-patched-source";
    inherit (tesseract) src;
    patches = import ./tesseract-patches.nix;
  };
  library = pkgs.stdenv.mkDerivation {
    pname = "dijo-tesseract";
    inherit (tesseract) version;
    src = ./tesseract-library;
    nativeBuildInputs = [
      pkgs.cmake
      pkgs.pkg-config
    ];
    buildInputs = [ pkgs.leptonica ];
    cmakeFlags = [ "-DTESSERACT_SOURCE=${source}" ];
    buildFlags = [ "dijo_tesseract" ];
    installPhase = ''
      mkdir -p $out
      cp out/*dijo_tesseract* $out/
    '';
    # Word images are kept as Pix (TESSERACT_IMAGEDATA_AS_PIX), not round-tripped through PNG: the
    # library has ImageData::SetPix and neither of the PNG round trip's functions. And it carries
    # the SIMD dot-product kernels tesseract picks from by the processor, not only the generic one.
    doInstallCheck = pkgs.stdenv.hostPlatform.isLinux;
    installCheckPhase = ''
      symbols=$(nm $out/*dijo_tesseract*)
      grep -q '_ZN9tesseract9ImageData6SetPixENS_5ImageE' <<<"$symbols"
      if grep -E 'ImageData14(Set|Get)PixInternal' <<<"$symbols"; then
        echo "tesseract was built without TESSERACT_IMAGEDATA_AS_PIX" >&2
        exit 1
      fi
      for kernel in ${
        lib.concatStringsSep " " (
          if pkgs.stdenv.hostPlatform.isx86_64 then
            [
              "intSimdMatrixAVX2"
              "intSimdMatrixSSE"
            ]
          else
            [ "intSimdMatrixNEON" ]
        )
      }; do
        if ! grep -q "IntSimdMatrix[0-9]*$kernel" <<<"$symbols"; then
          echo "tesseract was built without its $kernel kernel" >&2
          exit 1
        fi
      done
    '';
  };
  # Built without its CMake project, which finds Leptonica by the CMake package vcpkg installs and
  # nixpkgs' Leptonica lacks.
  heifPng = static.stdenv.mkDerivation {
    pname = "heif-png";
    version = "1";
    src = ../dijo-native/heif-png;
    nativeBuildInputs = [ pkgs.buildPackages.pkg-config ];
    buildInputs = [
      decoders.libheif
      static.leptonica
    ];
    buildPhase = ''
      $CXX -std=c++17 -O2 -static main.cc heif.cc -o heif-png \
        $(pkg-config --static --cflags --libs libheif lept)
    '';
    installPhase = ''
      mkdir -p $out
      cp heif-png $out/
    '';
  };
  linked = [
    tesseract
    pkgs.leptonica
    pkgs.stdenv.cc.cc
  ]
  ++ lib.optionals heifDecoded [
    decoders.libheif
    static.leptonica
    static.libde265
    static.dav1d
    static.stdenv.cc.libc
    static.stdenv.cc.cc
  ];
in
pkgs.runCommand "dijo-ocr-bundle-loaded" { } (
  ''
    mkdir -p $out/tessdata $out/licenses/tessdata_fast
    cp ${library}/* $out/
    cp -rL ${tessdata}/. $out/tessdata/
    cp ${tessdata.license} $out/licenses/tessdata_fast/LICENSE
  ''
  + lib.optionalString heifDecoded ''
    cp ${heifPng}/heif-png $out/
  ''
  + lib.concatMapStrings licenses (lib.unique linked)
)
