package ai.yobix;

import org.apache.tika.io.TemporaryResources;

import java.io.IOException;
import java.nio.file.Path;

/** DjVu pages rendered to images by DjVuLibre's {@code ddjvu}, when it is on the PATH. */
final class DjvuRenderer {

    private static final String PROGRAM = "ddjvu";

    private DjvuRenderer() {
    }

    /**
     * A page, counted from 1, rendered at full resolution as a PGM file, or null when ddjvu is
     * not installed.
     *
     * @throws IOException when ddjvu could not render the page in time
     */
    static Path render(Path djvu, int page, TemporaryResources temporary, long timeoutMillis)
            throws IOException {
        final Path image = temporary.createTempFile(".pgm");
        final String output;
        try {
            output = Programs.run(timeoutMillis, environment -> { }, PROGRAM, "-format=pgm",
                    "-page=" + page, djvu.toString(), image.toString());
        } catch (IOException notInstalled) {
            return null;
        }
        if (output == null) {
            throw new IOException(PROGRAM + " could not render page " + page + " in "
                    + timeoutMillis + " ms");
        }
        return image;
    }
}
