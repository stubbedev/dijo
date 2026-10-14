# tesseract built from a static package set for linking into one executable: without the
# training tools, curl or libarchive, and with OpenMP, so that a reading can run its recognizer on
# the threads ReadingThreads grants it; the recognizer asks for four threads, which the patch
# limits to the calling thread's omp_set_num_threads, or OMP_NUM_THREADS for the program.
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
      "--without-curl"
      "--without-archive"
      "--disable-doc"
    ];
    postPatch = (old.postPatch or "") + ''
      substituteInPlace src/lstm/fullyconnected.cpp --replace-fail "num_threads(kNumThreads)" \
        "num_threads(omp_get_max_threads() < kNumThreads ? omp_get_max_threads() : kNumThreads)"
    '';
    postInstall = (old.postInstall or "") + ''
      mkdir -p $out/include/tesseract-internal
      cp include/config_auto.h src/ccstruct/*.h src/ccutil/*.h src/viewer/*.h \
        $out/include/tesseract-internal/
    '';
  })
