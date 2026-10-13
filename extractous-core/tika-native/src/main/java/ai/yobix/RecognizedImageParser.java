package ai.yobix;

import org.apache.tika.exception.TikaException;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.HttpHeaders;
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
import java.util.stream.Collectors;
import java.util.stream.Stream;

/**
 * Images Tika reads for their metadata alone, or not at all, though they can be read for text: their
 * metadata as Tika reads it, and the text OCR finds in them. Text in WebP, PNM, Photoshop, PCX
 * and TGA images was lost.
 */
final class RecognizedImageParser implements Parser, EnrichingParser {

    static final MediaType WEBP = MediaType.image("webp");

    /** The images that tesseract reads as they are. */
    private static final Set<MediaType> READ_BY_TESSERACT = Set.of(WEBP,
            MediaType.image("x-portable-bitmap"), MediaType.image("x-portable-graymap"),
            MediaType.image("x-portable-pixmap"), MediaType.image("x-portable-anymap"));

    /** The images tesseract cannot read, which Java decodes for it. */
    static final Set<MediaType> DECODED_FOR_TESSERACT = Set.of(
            MediaType.image("vnd.adobe.photoshop"), MediaType.image("vnd.zbrush.pcx"),
            MediaType.image("x-tga"));

    static final Set<MediaType> TYPES = Stream.concat(READ_BY_TESSERACT.stream(),
            DECODED_FOR_TESSERACT.stream()).collect(Collectors.toUnmodifiableSet());

    private final MediaType type;
    private final Parser metadataParser;
    private CompositeContentEnricher recognizers;

    RecognizedImageParser(MediaType type, Parser metadataParser) {
        this.type = type;
        this.metadataParser = metadataParser;
    }

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return Set.of(type);
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
        final MediaType type = MediaType.parse(metadata.get(HttpHeaders.CONTENT_TYPE));
        final Parser recognizer = type == null
                ? null : ContentEnrichers.get(recognizers, type.getBaseType(), metadata, context);
        if (recognizer != null) {
            try (TikaInputStream image = TikaInputStream.get(path)) {
                recognizer.parse(image, new EmbeddedContentHandler(new BodyContentHandler(xhtml)),
                        metadata, context);
            }
        }
        xhtml.endDocument();
    }
}
