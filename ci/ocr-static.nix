# The OCR bundle of the static executables, which link tesseract themselves: the language data
# (ci/tessdata.nix) and its license, and the license files of the tesseract they link
# (ci/tesseract.nix) and of the static libraries it links, the C library and GCC's runtime.
{ nixpkgs, system }:
let
  pkgs = nixpkgs.legacyPackages.${system};
  lib = pkgs.lib;
  static = import ./static-packages.nix pkgs;
  tesseract = import ./tesseract.nix static;
  inputsOf =
    p:
    lib.filter (input: input != null && input ? src) (
      (p.buildInputs or [ ]) ++ (p.propagatedBuildInputs or [ ])
    );
  closure = lib.genericClosure {
    startSet = map (p: {
      key = p.name;
      package = p;
    }) (inputsOf tesseract);
    operator =
      item:
      map (p: {
        key = p.name;
        package = p;
      }) (inputsOf item.package);
  };
  tessdata = import ./tessdata.nix { inherit pkgs; };
  ppocr = import ./ppocr.nix { inherit pkgs; };
  licenses = import ./licenses.nix "$out/licenses";
in
pkgs.runCommand "dijo-ocr-bundle-linked" { } (
  ''
    mkdir -p $out/tessdata $out/ppocr $out/licenses/tessdata_fast $out/licenses/PaddleOCR
    cp -rL ${tessdata}/. $out/tessdata/
    cp ${tessdata.license} $out/licenses/tessdata_fast/LICENSE
    cp -rL ${ppocr}/. $out/ppocr/
    cp ${ppocr.license} $out/licenses/PaddleOCR/LICENSE
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
