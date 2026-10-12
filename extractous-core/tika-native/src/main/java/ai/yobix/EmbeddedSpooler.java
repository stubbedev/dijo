package ai.yobix;

import org.apache.tika.extractor.EmbeddedDocumentExtractor;
import org.apache.tika.metadata.HttpHeaders;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.metadata.TikaCoreProperties;
import org.xml.sax.ContentHandler;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.util.List;
import java.util.concurrent.CopyOnWriteArrayList;

/**
 * Copies every document embedded directly in the one being parsed to its own file in a spool
 * directory, instead of parsing it inline, up to a total byte budget.
 */
final class EmbeddedSpooler implements EmbeddedDocumentExtractor {

    private static final int COPY_BUFFER = 64 * 1024;

    private final Path directory;
    private final List<SpooledEmbedded> spooled = new CopyOnWriteArrayList<>();
    private long budget;

    EmbeddedSpooler(Path directory, long byteBudget) {
        this.directory = directory;
        this.budget = byteBudget;
    }

    List<SpooledEmbedded> spooled() {
        return spooled;
    }

    @Override
    public boolean shouldParseEmbedded(Metadata metadata) {
        return !ContentEmbeddedDocumentExtractor.isThumbnail(metadata);
    }

    @Override
    public void parseEmbedded(InputStream stream, ContentHandler handler, Metadata metadata,
                              boolean outputHtml) throws IOException {
        final int index = spooled.size();
        final String name = metadata.get(TikaCoreProperties.RESOURCE_NAME_KEY);
        final Path path = directory.resolve(String.valueOf(index));
        long size = 0;
        boolean truncated = false;
        try (OutputStream out = Files.newOutputStream(path, StandardOpenOption.CREATE_NEW)) {
            final byte[] buffer = new byte[COPY_BUFFER];
            int read;
            while ((read = stream.read(buffer)) != -1) {
                final int kept = (int) Math.min(read, budget);
                out.write(buffer, 0, kept);
                size += kept;
                budget -= kept;
                if (kept < read) {
                    truncated = true;
                    break;
                }
            }
        }
        spooled.add(new SpooledEmbedded(
                name == null ? "embedded-" + index : name,
                metadata.get(HttpHeaders.CONTENT_TYPE),
                path.toString(),
                size,
                truncated));
    }
}
