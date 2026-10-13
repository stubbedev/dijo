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
     * @throws IOException when heif-dec could not decode the image in time
     */
    static Path decode(Path image, TemporaryResources temporary, long timeoutMillis)
            throws IOException {
        final Path png = temporary.createTempFile(".png");
        final String output;
        try {
            output = Programs.run(timeoutMillis, environment -> { }, PROGRAM, "--quiet",
                    image.toString(), png.toString());
        } catch (IOException notInstalled) {
            return null;
        }
        if (output == null) {
            throw new IOException(PROGRAM + " could not decode the image in "
                    + timeoutMillis + " ms");
        }
        return png;
    }
}
