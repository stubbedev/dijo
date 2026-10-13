# A statically linked tesseract (musl), without the training tools, and the license files of
# everything linked into it: tesseract, the static libraries it links, the C library and GCC's
# runtime.
{ nixpkgs, system }:
let
  pkgs = nixpkgs.legacyPackages.${system};
  lib = pkgs.lib;
  static = pkgs.pkgsStatic;
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
  licenses = p: ''
    target=$out/licenses/${p.pname or p.name}
    mkdir -p "$target"
    unpacked=$(mktemp -d)
    if [ -d ${p.src} ]; then
      cp -r --no-preserve=mode ${p.src}/. "$unpacked"
    else
      tar -xf ${p.src} -C "$unpacked"
    fi
    root=$unpacked
    if [ "$(find "$unpacked" -mindepth 1 -maxdepth 1 | wc -l)" = 1 ]; then
      root=$(find "$unpacked" -mindepth 1 -maxdepth 1)
    fi
    find "$root" -maxdepth 1 -type f \( -iname '*licen[cs]e*' -o -iname 'COPYING*' \
      -o -iname 'COPYRIGHT*' -o -iname 'NOTICE*' -o -iname 'README.ijg' \) -exec cp {} "$target"/ \;
    if [ -z "$(ls -A "$target")" ]; then
      echo "no license file found in the source of ${p.name}" >&2
      exit 1
    fi
    rm -rf "$unpacked"
  '';
in
pkgs.runCommand "tesseract-static-bundle" { } (
  ''
    mkdir -p $out/licenses
    cp ${tesseract}/bin/tesseract $out/tesseract
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
