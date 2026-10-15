# The static package set the static builds take their libraries from (ci/ocr-static.nix,
# ci/static-toolchain.nix): nixpkgs' pkgsStatic, with Leptonica compiled at -O3 rather than its
# autotools -O2, which reads pages 4.5% faster with the same text.
pkgs:
pkgs.pkgsStatic.extend (
  final: prev: {
    leptonica = prev.leptonica.overrideAttrs (old: {
      env = (old.env or { }) // {
        CFLAGS = "-O3";
      };
    });
  }
)
