package ai.yobix;

import org.apache.tika.exception.TikaException;
import org.apache.tika.exception.UnsupportedFormatException;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.io.TemporaryResources;
import org.apache.tika.metadata.TikaCoreProperties;
import org.apache.tika.metadata.TikaPagedText;
import org.apache.tika.parser.enricher.CompositeContentEnricher;
import org.apache.tika.parser.enricher.ContentEnrichers;
import org.apache.tika.parser.enricher.EnrichingParser;
import org.apache.tika.parser.ocr.TesseractOCRConfig;
import org.apache.tika.sax.BodyContentHandler;
import org.apache.tika.sax.EmbeddedContentHandler;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.metadata.PagedText;
import org.apache.tika.mime.MediaType;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.sax.XHTMLContentHandler;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.channels.FileChannel;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.util.ArrayList;
import java.util.List;
import java.util.Set;
import java.util.regex.Pattern;

/**
 * DjVu documents, single-page or bundled multi-page, read for the hidden text layer each page
 * carries (TXTa, or TXTz compressed with BZZ): a page of the output per page of the document,
 * its text split into paragraphs where the layer ends a column, region or paragraph. A page
 * without a text layer is rendered by DjVuLibre's ddjvu, when it is installed, and read by OCR.
 * Tika detects DjVu but has no parser for it.
 */
final class DjvuParser implements Parser, EnrichingParser {

    private static final MediaType RENDERED = MediaType.image("x-portable-graymap");
    private static final TesseractOCRConfig DEFAULTS = new OcrConfig();

    private CompositeContentEnricher recognizers;

    @Override
    public void setContentEnrichers(CompositeContentEnricher recognizers) {
        this.recognizers = recognizers;
    }

    static final MediaType DJVU = MediaType.image("vnd.djvu");

    private static final int CHUNK_HEADER = 8;
    private static final int MAGIC = 0x41542654;
    private static final int MAX_TEXT_BYTES = 64 << 20;
    private static final int BUNDLED = 0x80;
    private static final Pattern BLOCK_END = Pattern.compile("[\\u000b\\u001d\\u001f]+");

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return Set.of(DJVU);
    }

    @Override
    public void parse(TikaInputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        final Path path = TikaInputStream.get(stream).getPath();
        final List<String> pages;
        try (FileChannel file = FileChannel.open(path, StandardOpenOption.READ)) {
            pages = pages(file);
        }
        metadata.set(PagedText.N_PAGES, pages.size());
        final XHTMLContentHandler xhtml = new XHTMLContentHandler(handler, metadata);
        xhtml.startDocument();
        try (TemporaryResources temporary = new TemporaryResources()) {
            for (int index = 0; index < pages.size(); index++) {
                final String page = pages.get(index);
                xhtml.startElement("div", "class", "page");
                if (page.isBlank()) {
                    readByOcr(path, index + 1, xhtml, metadata, context, temporary);
                }
                for (final String block : BLOCK_END.split(page)) {
                    if (!block.isBlank()) {
                        xhtml.element("p", block.strip());
                    }
                }
                xhtml.endElement("div");
            }
        }
        xhtml.endDocument();
    }

    /** Reads a page without a text layer by OCR, when OCR is on and ddjvu can render it. */
    private void readByOcr(Path djvu, int page, XHTMLContentHandler xhtml, Metadata metadata,
                           ParseContext context, TemporaryResources temporary)
            throws IOException, SAXException, TikaException {
        final TesseractOCRConfig configured = context.get(TesseractOCRConfig.class);
        if (configured != null && configured.isSkipOcr()) {
            return;
        }
        final Metadata pageMetadata = new Metadata();
        pageMetadata.set(TikaCoreProperties.CONTENT_TYPE_PARSER_OVERRIDE, RENDERED.toString());
        pageMetadata.set(TikaPagedText.PAGE_NUMBER, page);
        final Parser recognizer = ContentEnrichers.get(recognizers, RENDERED, pageMetadata,
                context);
        if (recognizer == null) {
            return;
        }
        final Path image = DjvuRenderer.render(djvu, page, temporary,
                (configured != null ? configured : DEFAULTS).getTimeoutMillis());
        if (image == null) {
            metadata.add(TikaCoreProperties.TIKA_META_EXCEPTION_WARNING, "Page " + page
                    + " has no text layer; reading it by OCR needs DjVuLibre's ddjvu on the PATH");
            return;
        }
        try (TikaInputStream rendered = TikaInputStream.get(image)) {
            recognizer.parse(rendered, new EmbeddedContentHandler(new BodyContentHandler(xhtml)),
                    pageMetadata, context);
        }
        for (final String report : pageMetadata.getValues(OcrReport.KEY)) {
            metadata.add(OcrReport.KEY, report);
        }
    }

    /** The text of each page, in order, empty for a page without a text layer. */
    private static List<String> pages(FileChannel file) throws IOException, TikaException {
        if (file.size() < 4 + CHUNK_HEADER + 4 || readInt(file, 0) != MAGIC) {
            throw new TikaException("Not a DjVu file: no AT&T magic");
        }
        final Chunk root = chunk(file, 4, file.size());
        if (!"FORM".equals(root.id)) {
            throw new TikaException("A DjVu file starts with a FORM chunk, not " + root.id);
        }
        final String kind = readId(file, root.data);
        final List<String> pages = new ArrayList<>();
        if ("DJVU".equals(kind)) {
            pages.add(pageText(file, root.data + 4, root.end));
            return pages;
        }
        if (!"DJVM".equals(kind)) {
            throw new TikaException("Unknown DjVu FORM type " + kind);
        }
        for (long offset = root.data + 4; offset < root.end; ) {
            final Chunk chunk = chunk(file, offset, root.end);
            if ("DIRM".equals(chunk.id) && chunk.size > 0
                    && (readByte(file, chunk.data) & BUNDLED) == 0) {
                throw new UnsupportedFormatException(
                        "An indirect DjVu document keeps its pages in other files");
            }
            if ("FORM".equals(chunk.id) && chunk.size >= 4
                    && "DJVU".equals(readId(file, chunk.data))) {
                pages.add(pageText(file, chunk.data + 4, chunk.end));
            }
            offset = chunk.next;
        }
        return pages;
    }

    /** The text of the page whose chunks lie between `start` and `end`. */
    private static String pageText(FileChannel file, long start, long end)
            throws IOException, TikaException {
        final StringBuilder text = new StringBuilder();
        for (long offset = start; offset < end; ) {
            final Chunk chunk = chunk(file, offset, end);
            if ("TXTa".equals(chunk.id) || "TXTz".equals(chunk.id)) {
                if (chunk.size > MAX_TEXT_BYTES) {
                    throw new TikaException("A DjVu text chunk of " + chunk.size
                            + " bytes exceeds " + MAX_TEXT_BYTES);
                }
                final byte[] data = read(file, chunk.data, (int) chunk.size);
                text.append(layerText("TXTz".equals(chunk.id)
                        ? BzzDecoder.decode(data, MAX_TEXT_BYTES) : data));
            }
            offset = chunk.next;
        }
        return text.toString();
    }

    /** The UTF-8 string a text layer begins with, after its 24-bit length (8.3.5). */
    private static String layerText(byte[] layer) throws TikaException {
        if (layer.length == 0) {
            return "";
        }
        if (layer.length < 3) {
            throw new TikaException("A DjVu text layer ends inside its length");
        }
        final int length = ((layer[0] & 0xff) << 16) | ((layer[1] & 0xff) << 8)
                | (layer[2] & 0xff);
        if (length > layer.length - 3) {
            throw new TikaException("A DjVu text layer is shorter than its length of " + length);
        }
        return new String(layer, 3, length, StandardCharsets.UTF_8);
    }

    /** An IFF chunk: its id, where its data starts, its size, its end and the next chunk. */
    private record Chunk(String id, long data, long size, long end, long next) {
    }

    private static Chunk chunk(FileChannel file, long offset, long limit)
            throws IOException, TikaException {
        if (offset + CHUNK_HEADER > limit) {
            throw new TikaException("A DjVu chunk header at " + offset + " passes its container");
        }
        final String id = readId(file, offset);
        final long size = readInt(file, offset + 4) & 0xffffffffL;
        final long data = offset + CHUNK_HEADER;
        if (data + size > limit) {
            throw new TikaException("The DjVu chunk " + id + " at " + offset
                    + " passes its container");
        }
        final long end = data + size;
        return new Chunk(id, data, size, end, Math.min(limit, end + (end & 1)));
    }

    private static String readId(FileChannel file, long offset) throws IOException, TikaException {
        return new String(read(file, offset, 4), StandardCharsets.US_ASCII);
    }

    private static int readInt(FileChannel file, long offset) throws IOException, TikaException {
        return ByteBuffer.wrap(read(file, offset, 4)).getInt();
    }

    private static int readByte(FileChannel file, long offset) throws IOException, TikaException {
        return read(file, offset, 1)[0] & 0xff;
    }

    private static byte[] read(FileChannel file, long offset, int length)
            throws IOException, TikaException {
        final ByteBuffer buffer = ByteBuffer.allocate(length);
        while (buffer.hasRemaining()) {
            if (file.read(buffer, offset + buffer.position()) < 0) {
                throw new TikaException("The DjVu file ends inside a chunk at " + offset);
            }
        }
        return buffer.array();
    }
}
