package ai.yobix;

import org.apache.tika.exception.TikaException;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.mime.MediaType;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.parser.enricher.CompositeContentEnricher;
import org.apache.tika.parser.enricher.ContentEnrichers;
import org.apache.tika.parser.enricher.EnrichingParser;
import org.apache.tika.sax.BodyContentHandler;
import org.apache.tika.sax.EmbeddedContentHandler;
import org.apache.tika.sax.XHTMLContentHandler;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import java.io.IOException;
import java.nio.file.Path;
import java.util.Set;

/**
 * WebP images: their metadata as Tika reads it, and the text OCR finds in them. Tika reads WebP
 * for its metadata alone and never hands it to OCR, though tesseract reads WebP, so text in a
 * WebP image was lost.
 */
final class WebpParser implements Parser, EnrichingParser {

    static final MediaType WEBP = MediaType.image("webp");

    private final Parser metadataParser;
    private CompositeContentEnricher recognizers;

    WebpParser(Parser metadataParser) {
        this.metadataParser = metadataParser;
    }

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return Set.of(WEBP);
    }

    @Override
    public void setContentEnrichers(CompositeContentEnricher recognizers) {
        this.recognizers = recognizers;
    }

    @Override
    public void parse(TikaInputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        final Path path = stream.getPath();
        final XHTMLContentHandler xhtml = new XHTMLContentHandler(handler, metadata, context);
        xhtml.startDocument();
        try (TikaInputStream image = TikaInputStream.get(path)) {
            metadataParser.parse(image, new EmbeddedContentHandler(xhtml), metadata, context);
        }
        final Parser recognizer = ContentEnrichers.get(recognizers, WEBP, metadata, context);
        if (recognizer != null) {
            try (TikaInputStream image = TikaInputStream.get(path)) {
                recognizer.parse(image, new EmbeddedContentHandler(new BodyContentHandler(xhtml)),
                        metadata, context);
            }
        }
        xhtml.endDocument();
    }
}
