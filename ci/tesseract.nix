# tesseract built from a static package set for linking into one executable: without the
# training tools, OpenMP (parallelism comes from reading many images at once), curl or libarchive.
# Its internal headers and the configuration it was built with are installed in
# include/tesseract-internal, for code that reads its results while it recognizes a page.
static:
(static.tesseract5.passthru.tesseractBase.override {
  pango = null;
  icu = null;
  curl = null;
  libarchive = null;
}).overrideAttrs
  (old: {
    configureFlags = (old.configureFlags or [ ]) ++ [
      "--disable-openmp"
      "--without-curl"
      "--without-archive"
      "--disable-doc"
    ];
    postInstall = (old.postInstall or "") + ''
      mkdir -p $out/include/tesseract-internal
      cp include/config_auto.h src/ccstruct/*.h src/ccutil/*.h src/viewer/*.h \
        $out/include/tesseract-internal/
    '';
  })
