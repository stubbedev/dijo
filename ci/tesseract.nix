# tesseract built from a static package set for linking into one executable: without the
# training tools, OpenMP (parallelism comes from reading many images at once), curl or libarchive.
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
  })
