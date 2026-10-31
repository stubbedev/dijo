{ pkgs }:
let
  inherit (pkgs) lib;
  pinned = import ./pinned.nix lib ./ppocr.env;
  rows = builtins.filter (line: line != "") (lib.splitString "\n" (builtins.readFile ./ppocr.tsv));
  model =
    row:
    let
      field = builtins.elemAt (lib.splitString "\t" row);
    in
    {
      name = field 0;
      path = pkgs.fetchurl {
        url = field 1;
        sha256 = field 2;
      };
    };
in
(pkgs.linkFarm "ppocr" (map model rows)).overrideAttrs {
  passthru.license = pkgs.fetchurl {
    url = "https://raw.githubusercontent.com/PaddlePaddle/PaddleOCR/${pinned.PADDLEOCR_COMMIT}/LICENSE";
    sha256 = pinned.PADDLEOCR_LICENSE_SHA256;
  };
}
