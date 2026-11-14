# The static package set the static builds take their libraries from (ci/ocr-static.nix,
# ci/static-toolchain.nix): nixpkgs' pkgsStatic, with Leptonica compiled at -O3 rather than its
# autotools -O2, which reads pages 4.5% faster with the same text, and tesseract the release
# ci/vcpkg.env pins for the bundled library (ci/ocr-vcpkg.sh), from the same source archive,
# whichever release nixpkgs carries: the patches every build applies (ci/tesseract-patches.nix) are
# made against it. The development shell's bundle (ci/ocr-loaded.nix) builds its tesseractSource.
pkgs:
pkgs.pkgsStatic.extend (
  final: prev: {
    tesseractSource =
      let
        pinned = import ./pinned.nix prev.lib ./vcpkg.env;
        version = pinned.TESSERACT_VERSION;
      in
      {
        inherit version;
        src = pkgs.fetchurl {
          url = "https://github.com/tesseract-ocr/tesseract/archive/${version}.tar.gz";
          sha512 = pinned.TESSERACT_SHA512;
        };
      };
    tesseract5 = prev.tesseract5.override {
      tesseractBase = prev.tesseract5.passthru.tesseractBase.overrideAttrs final.tesseractSource;
    };
    leptonica = prev.leptonica.overrideAttrs (old: {
      env = (old.env or { }) // {
        CFLAGS = "-O3";
      };
    });
  }
)
