# The image decoder the static build links in place of libheif's heif-dec, built from a static
# package set: libheif with only its HEVC (libde265) and AV1 (dav1d) decoders.
static: {
  libheif =
    (static.libheif.override {
      rav1e = null;
      x265 = null;
      libaom = null;
      gdk-pixbuf = null;
      libpng = null;
      libjpeg = null;
    }).overrideAttrs
      (old: {
        outputs = [
          "out"
          "dev"
        ];
        cmakeFlags = (old.cmakeFlags or [ ]) ++ [
          "-DWITH_EXAMPLES=OFF"
          "-DWITH_GDK_PIXBUF=OFF"
          "-DBUILD_TESTING=OFF"
          "-DENABLE_PLUGIN_LOADING=OFF"
          "-DWITH_LIBDE265=ON"
          "-DWITH_DAV1D=ON"
        ];
        env = removeAttrs old.env [ "PKG_CONFIG_GDK_PIXBUF_2_0_GDK_PIXBUF_MODULEDIR" ];
        postInstall = "";
      });
}
