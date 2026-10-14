package ai.yobix;

import org.apache.tika.exception.TikaException;
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
import java.nio.channels.FileChannel;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.util.List;
import java.util.Set;
import java.util.regex.Pattern;

/**
 * DjVu documents, single-page or bundled multi-page, read for the hidden text layer each page
 * carries (TXTa, or TXTz compressed with BZZ): a page of the output per page of the document,
 * its text split into paragraphs where the layer ends a column, region or paragraph. A page
 * without a text layer is rendered (DjvuRenderer) and read by OCR.
 * Tika detects DjVu but has no parser for it.
 */
final class DjvuParser implements Parser, EnrichingParser {

    private static final MediaType RENDERED = MediaType.image("x-portable-graymap");

    private CompositeContentEnricher recognizers;

    @Override
    public void setContentEnrichers(CompositeContentEnricher recognizers) {
        this.recognizers = recognizers;
    }

    static final MediaType DJVU = MediaType.image("vnd.djvu");

    private static final int MAX_TEXT_BYTES = 64 << 20;
    private static final Pattern BLOCK_END = Pattern.compile("[\\u000b\\u001d\\u001f]+");

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return Set.of(DJVU);
    }

    @Override
    public void parse(TikaInputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        final Path path = TikaInputStream.get(stream).getPath();
        try (FileChannel file = FileChannel.open(path, StandardOpenOption.READ);
             TemporaryResources temporary = new TemporaryResources()) {
            final DjvuDocument document = DjvuDocument.read(file);
            final List<DjvuDocument.Form> pages = document.pages();
            metadata.set(PagedText.N_PAGES, pages.size());
            final XHTMLContentHandler xhtml = new XHTMLContentHandler(handler, metadata);
            xhtml.startDocument();
            for (int index = 0; index < pages.size(); index++) {
                final String page = pageText(document, pages.get(index));
                xhtml.startElement("div", "class", "page");
                if (page.isBlank()) {
                    readByOcr(document, index + 1, xhtml, metadata, context, temporary);
                }
                for (final String block : BLOCK_END.split(page)) {
                    if (!block.isBlank()) {
                        xhtml.element("p", block.strip());
                    }
                }
                xhtml.endElement("div");
            }
            xhtml.endDocument();
        }
    }

    /** Reads a page without a text layer by OCR, when OCR is on. */
    private void readByOcr(DjvuDocument document, int page, XHTMLContentHandler xhtml,
                           Metadata metadata, ParseContext context, TemporaryResources temporary)
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
        final Path image = DjvuRenderer.render(document, page, temporary);
        try (TikaInputStream rendered = TikaInputStream.get(image)) {
            recognizer.parse(rendered, new EmbeddedContentHandler(new BodyContentHandler(xhtml)),
                    pageMetadata, context);
        }
        for (final String report : pageMetadata.getValues(OcrReport.KEY)) {
            metadata.add(OcrReport.KEY, report);
        }
    }

    /** The text of a page's text layer, empty when it has none. */
    private static String pageText(DjvuDocument document, DjvuDocument.Form page)
            throws IOException, TikaException {
        final StringBuilder text = new StringBuilder();
        for (final DjvuDocument.Chunk chunk : document.chunks(page)) {
            if ("TXTa".equals(chunk.id()) || "TXTz".equals(chunk.id())) {
                if (chunk.size() > MAX_TEXT_BYTES) {
                    throw new TikaException("A DjVu text chunk of " + chunk.size()
                            + " bytes exceeds " + MAX_TEXT_BYTES);
                }
                final byte[] data = document.data(chunk);
                text.append(layerText("TXTz".equals(chunk.id())
                        ? BzzDecoder.decode(data, MAX_TEXT_BYTES) : data));
            }
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
}
