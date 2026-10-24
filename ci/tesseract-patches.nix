# The patches every build of tesseract carries, ci/tesseract-patches/*.patch in the order of their
# names: the static executable's (ci/tesseract.nix), the bundled library's (ci/ocr-vcpkg.sh) and
# the development shell's bundle (ci/ocr-loaded.nix), so that tests read with the tesseract the
# builds ship.
let
  dir = ./tesseract-patches;
  names = builtins.filter (name: builtins.match ".*\\.patch" name != null) (
    builtins.attrNames (builtins.readDir dir)
  );
in
map (name: dir + "/${name}") (builtins.sort builtins.lessThan names)
