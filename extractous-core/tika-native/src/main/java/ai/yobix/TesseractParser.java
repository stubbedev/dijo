package ai.yobix;

import org.apache.tika.exception.TikaException;
import org.apache.tika.io.TemporaryResources;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.mime.MediaType;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.parser.enricher.TextRecognizer;
import org.apache.tika.parser.ocr.TesseractOCRConfig;
import org.apache.tika.sax.XHTMLContentHandler;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import java.io.IOException;
import java.nio.file.Path;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Set;

/**
 * Reads the text in an image with tesseract, as Tika's tesseract parser does with its text
 * output: in a {@code div} of class {@code ocr}, configured by the {@link TesseractOCRConfig} in
 * the parse context.
 */
final class TesseractParser implements Parser, TextRecognizer {

    private static final Set<MediaType> TYPES = Set.of(MediaType.image("png"),
            MediaType.image("jpeg"), MediaType.image("tiff"), MediaType.image("bmp"),
            MediaType.image("gif"), MediaType.image("jp2"), MediaType.image("jpx"),
            MediaType.image("x-portable-pixmap"));
    private static final TesseractOCRConfig DEFAULTS = new TesseractOCRConfig();

    private final Tesseract tesseract;

    TesseractParser(Tesseract tesseract) {
        this.tesseract = tesseract;
    }

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return config(context).isSkipOcr() ? Collections.emptySet() : TYPES;
    }

    @Override
    public boolean recognizesText(ParseContext context) {
        return !config(context).isSkipOcr();
    }

    @Override
    public void parse(TikaInputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        final TesseractOCRConfig config = config(context);
        if (config.isSkipOcr()) {
            return;
        }
        try (TemporaryResources temporary = new TemporaryResources()) {
            final TikaInputStream image = TikaInputStream.get(stream, temporary, metadata);
            final Path path = image.getPath();
            final XHTMLContentHandler xhtml = new XHTMLContentHandler(handler, metadata, context);
            xhtml.startDocument();
            final long size = image.getLength();
            if (size >= config.getMinFileSizeToOcr() && size <= config.getMaxFileSizeToOcr()) {
                final String text = tesseract.read(path, config.getPageSegMode(),
                        config.getLanguage(), variables(config), config.getTimeoutMillis());
                xhtml.startElement("div", "class", "ocr");
                xhtml.characters(text);
                xhtml.endElement("div");
            }
            xhtml.endDocument();
        }
    }

    private static TesseractOCRConfig config(ParseContext context) {
        final TesseractOCRConfig configured = context.get(TesseractOCRConfig.class);
        return configured != null ? configured : DEFAULTS;
    }

    private static Map<String, String> variables(TesseractOCRConfig config) {
        final Map<String, String> variables = new LinkedHashMap<>(
                config.getOtherTesseractConfig());
        variables.put("page_separator", config.getPageSeparator());
        variables.put("preserve_interword_spaces", config.isPreserveInterwordSpacing() ? "1" : "0");
        return variables;
    }
}
