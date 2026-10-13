package ai.yobix;

import org.apache.tika.io.TemporaryResources;

import java.io.IOException;
import java.nio.file.Path;

/**
 * HEIC, HEIF and AVIF images decoded to PNG by libheif's {@code heif-dec}, when it is on the
 * PATH, since neither Java nor tesseract can decode them.
 */
final class HeifDecoder {

    private static final String PROGRAM = "heif-dec";

    private HeifDecoder() {
    }

    /**
     * The image as a PNG file, or null when heif-dec is not installed.
     *
     * @throws IOException when heif-dec could not decode the image
     */
    static Path decode(Path image, TemporaryResources temporary, long timeoutMillis)
            throws IOException {
        final Path png = temporary.createTempFile(".png");
        final Programs.Outcome decoding;
        try {
            decoding = Programs.run(timeoutMillis, environment -> { }, PROGRAM, "--quiet",
                    image.toString(), png.toString());
        } catch (IOException notInstalled) {
            return null;
        }
        if (!decoding.succeeded()) {
            throw new IOException("Decoding the image for OCR failed: " + decoding.failure());
        }
        return png;
    }
}
