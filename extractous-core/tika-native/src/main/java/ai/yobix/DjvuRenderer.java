package ai.yobix;

import org.apache.tika.io.TemporaryResources;

import java.io.IOException;
import java.nio.file.Path;

/**
 * DjVu pages rendered to images by DjVuLibre: linked into the static executable, or else its
 * {@code ddjvu} when it is on the PATH.
 */
final class DjvuRenderer {

    private static final String PROGRAM = "ddjvu";

    private DjvuRenderer() {
    }

    /**
     * A page, counted from 1, rendered at full resolution as a PGM file, or null when DjVuLibre
     * is neither linked nor installed.
     *
     * @throws IOException when DjVuLibre could not render the page
     */
    static Path render(Path djvu, int page, TemporaryResources temporary, long timeoutMillis)
            throws IOException {
        final Path image = temporary.createTempFile(".pgm");
        if (StaticImage.running()) {
            LinkedImages.renderDjvu(djvu, page, image);
            return image;
        }
        final Programs.Outcome rendering;
        try {
            rendering = Programs.run(timeoutMillis, environment -> { }, PROGRAM, "-format=pgm",
                    "-page=" + page, djvu.toString(), image.toString());
        } catch (IOException notInstalled) {
            return null;
        }
        if (!rendering.succeeded()) {
            throw new IOException("Rendering page " + page + " for OCR failed: "
                    + rendering.failure());
        }
        return image;
    }
}
