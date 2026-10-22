old: {
  patches = (old.patches or [ ]) ++ import ./tesseract-patches.nix;
  env = (old.env or { }) // {
    CPPFLAGS = "-DTESSERACT_IMAGEDATA_AS_PIX";
  };
  configureFlags = (old.configureFlags or [ ]) ++ [ "--disable-graphics" ];
}
