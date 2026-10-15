package ai.yobix;

import org.apache.tika.io.TemporaryResources;

import java.io.IOException;
import java.nio.file.Path;

/**
 * HEIC, HEIF and AVIF images decoded to PNG, since neither Java nor tesseract can decode them: by
 * libheif linked into the static executable, by macOS's own {@code sips}, by the heif-png the OCR
 * bundle carries, or else by libheif's {@code heif-dec} when it is on the PATH.
 */
final class HeifDecoder {

    private static final String PROGRAM = "heif-dec";
    /**
     * Where libheif loads decoder plugins from, which the bundled heif-png, linked statically with
     * its own decoders, must not load: a plugin built for another libheif and C library.
     */
    private static final String PLUGIN_PATH = "LIBHEIF_PLUGIN_PATH";
    private static final String MAC_OS_PROGRAM = "/usr/bin/sips";
    private static final boolean MAC_OS = System.getProperty("os.name").startsWith("Mac");

    private HeifDecoder() {
    }

    /**
     * The image as a PNG file, or null when no decoder is linked, built in, bundled or installed.
     *
     * @throws IOException when libheif could not decode the image
     */
    static Path decode(Path image, TemporaryResources temporary, long timeoutMillis)
            throws IOException {
        final Path png = temporary.createTempFile(".png");
        if (StaticImage.running()) {
            LinkedImages.decodeHeif(image, png);
            return png;
        }
        if (MAC_OS) {
            return decoded(Programs.run(timeoutMillis, environment -> { }, MAC_OS_PROGRAM, "-s",
                    "format", "png", image.toString(), "--out", png.toString()), png);
        }
        final Path bundled = OcrBundle.heifDecoder();
        if (bundled != null) {
            return decoded(Programs.run(timeoutMillis,
                    environment -> environment.remove(PLUGIN_PATH), bundled.toString(),
                    image.toString(), png.toString()), png);
        }
        final Programs.Outcome decoding;
        try {
            decoding = Programs.run(timeoutMillis, environment -> { }, PROGRAM, "--quiet",
                    image.toString(), png.toString());
        } catch (IOException notInstalled) {
            return null;
        }
        return decoded(decoding, png);
    }

    private static Path decoded(Programs.Outcome decoding, Path png) throws IOException {
        if (!decoding.succeeded()) {
            throw new IOException("Decoding the image for OCR failed: " + decoding.failure());
        }
        return png;
    }
}
