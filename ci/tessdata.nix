# The tesseract language models of ci/tessdata.tsv, from tessdata_fast at the commit in
# ci/tessdata.env, as one tessdata directory: the models devenv and CI read with, the same the
# OCR bundle carries.
{ pkgs }:
let
  inherit (pkgs) lib;
  commit = builtins.head (builtins.match ".*TESSDATA_COMMIT=([0-9a-f]+).*" (builtins.readFile ./tessdata.env));
  rows = builtins.filter (line: line != "" && !(lib.hasPrefix "#" line))
    (lib.splitString "\n" (builtins.readFile ./tessdata.tsv));
  model = row:
    let
      fields = lib.splitString "\t" row;
      path = builtins.elemAt fields 0;
    in
    {
      name = "${path}.traineddata";
      path = pkgs.fetchurl {
        url = "https://raw.githubusercontent.com/tesseract-ocr/tessdata_fast/${commit}/${path}.traineddata";
        sha256 = builtins.elemAt fields 1;
      };
    };
in
pkgs.linkFarm "tessdata-fast" (map model rows)
