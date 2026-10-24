# tesseract built from a static package set for linking into one executable: without the
# training tools, curl or libarchive, and without OpenMP, as tesseract's CMake build and the
# bundles' library are. Readings run side by side, a page per thread; an OpenMP team per reading
# spent 2.6 times the processor time for a 1.2 times faster lone page, and a worker of one aborted
# the static server once (#62). The StrideMap index methods the network calls for every time step are
# made inline (tesseract-patches/inline-stridemap.patch): out of line they took a fifth of a page's
# instructions, and inline a page reads 10 to 13% faster with the same text. It is compiled at -O3,
# as CMake's release builds of it are, where its autotools build sets -O2: 5% faster again. A model
# file is mapped read-only and its dictionaries, almost all of its bytes (88.8 of script/Latin's
# 89.4 MB), are read in place rather than copied (tesseract-patches/mmap-models.patch): every
# process reading with a model shares one copy of them in the page cache, and loading one copies
# nothing large, so reading a Latin page peaks at 191 MB instead of 305 MB (#211). An engine keeps
# no empty adapted class for each unichar unless its legacy classifier runs and learns
# (tesseract-patches/adapts-only-while-learning.patch), and one for an LSTM model loads no universal
# ambiguities into its unicharset (tesseract-patches/lstm-skips-universal-ambigs.patch): a chi_sim
# engine takes 9 MiB instead of 43, an eng one 2 instead of 10, and the orientation and script
# engine 19 instead of 32 (#308, #311). Its result iterators return to the start of a row or a
# paragraph from where they entered it (tesseract-patches/rows-restart-in-place.patch), where they
# walked the page from its start, once a line: a page tesseract splits into many small words, such
# as one of noise, never finished its paragraph detection, and now reads in seconds (#316).
# Its internal headers and the configuration it was built with are installed in
# include/tesseract-internal, for code that reads its results while it recognizes a page.
static:
(
  (static.tesseract5.passthru.tesseractBase.override {
    pango = null;
    icu = null;
    curl = null;
    libarchive = null;
  }).overrideAttrs
    (import ./tesseract-build.nix)
).overrideAttrs
  (old: {
    env = (old.env or { }) // {
      CXXFLAGS = "-O3";
    };
    configureFlags = (old.configureFlags or [ ]) ++ [
      "--without-curl"
      "--without-archive"
      "--disable-doc"
      "--disable-openmp"
    ];
    postInstall = (old.postInstall or "") + ''
      mkdir -p $out/include/tesseract-internal
      cp include/config_auto.h src/ccstruct/*.h src/ccutil/*.h src/viewer/*.h \
        $out/include/tesseract-internal/
      echo '#define TESSERACT_IMAGEDATA_AS_PIX 1' >> $out/include/tesseract-internal/config_auto.h
    '';
  })
