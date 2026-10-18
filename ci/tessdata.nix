# The tesseract language models of ci/tessdata.tsv, from tessdata_fast at the commit in
# ci/tessdata.env, as one tessdata directory: the models devenv and CI read with, the same the
# OCR bundle carries, with tessdata_fast's license as passthru.license.
{ pkgs }:
let
  inherit (pkgs) lib;
  pinned = lib.listToAttrs (
    map (
      line:
      let
        pair = builtins.match "([A-Z0-9_]+)=([0-9a-f]+)" line;
      in
      lib.nameValuePair (builtins.elemAt pair 0) (builtins.elemAt pair 1)
    ) (builtins.filter (line: line != "") (lib.splitString "\n" (builtins.readFile ./tessdata.env)))
  );
  base = "https://raw.githubusercontent.com/tesseract-ocr/tessdata_fast/${pinned.TESSDATA_COMMIT}";
  rows = builtins.filter (line: line != "" && !(lib.hasPrefix "#" line)) (
    lib.splitString "\n" (builtins.readFile ./tessdata.tsv)
  );
  model =
    row:
    let
      fields = lib.splitString "\t" row;
      path = builtins.elemAt fields 0;
    in
    {
      name = "${path}.traineddata";
      path = pkgs.fetchurl {
        url = "${base}/${path}.traineddata";
        sha256 = builtins.elemAt fields 1;
      };
    };
in
(pkgs.linkFarm "tessdata-fast" (map model rows)).overrideAttrs {
  passthru.license = pkgs.fetchurl {
    url = "${base}/LICENSE";
    sha256 = pinned.TESSDATA_LICENSE_SHA256;
  };
}
