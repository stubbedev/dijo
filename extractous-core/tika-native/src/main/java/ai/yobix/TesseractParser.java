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
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Optional;
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
    private static final TesseractOCRConfig DEFAULTS = new OcrConfig();

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
            final XHTMLContentHandler xhtml = new XHTMLContentHandler(handler, metadata, context);
            xhtml.startDocument();
            if (withinLimits(image.getLength(), config)) {
                write(xhtml, read(new OcrImage.File(image.getPath()), config).text());
            }
            xhtml.endDocument();
        }
    }

    /** Whether an image whose file has a size is to be read, within the configured limits. */
    static boolean withinLimits(long size, TesseractOCRConfig config) {
        return size >= config.getMinFileSizeToOcr() && size <= config.getMaxFileSizeToOcr();
    }

    /** Reads an image as configured. */
    Tesseract.Reading read(OcrImage image, TesseractOCRConfig config)
            throws IOException, TikaException {
        return read(image, config, config.getLanguage(), Map.of());
    }

    /**
     * Reads an image as configured, but in a language and with variables set for this image,
     * which leave the configuration, shared by the images of an extraction, as it is.
     */
    Tesseract.Reading read(OcrImage image, TesseractOCRConfig config, String language,
                           Map<String, String> variables) throws IOException, TikaException {
        return corrected(tesseract.read(image, config.getPageSegMode(), language,
                variables(config, variables), config.getTimeoutMillis()));
    }

    /** Reads as {@link #read} does, unless the reading goes poorly: empty then. */
    Optional<Tesseract.Reading> readUnlessPoor(OcrImage image, TesseractOCRConfig config,
                                               String language, Map<String, String> variables,
                                               Tesseract.Poor poor)
            throws IOException, TikaException {
        final Optional<Tesseract.Reading> reading = tesseract.readUnlessPoor(image,
                config.getPageSegMode(), language, variables(config, variables), poor,
                config.getTimeoutMillis());
        return reading.map(TesseractParser::corrected);
    }

    private static Tesseract.Reading corrected(Tesseract.Reading reading) {
        return new Tesseract.Reading(OcrText.corrected(reading.text()), reading.confidence());
    }

    /** Writes text read from an image as Tika does: in a div of class ocr. */
    static void write(XHTMLContentHandler xhtml, String text) throws SAXException {
        xhtml.startElement("div", "class", "ocr");
        xhtml.characters(text);
        xhtml.endElement("div");
    }

    private static TesseractOCRConfig config(ParseContext context) {
        final TesseractOCRConfig configured = context.get(TesseractOCRConfig.class);
        return configured != null ? configured : DEFAULTS;
    }

    private static Map<String, String> variables(TesseractOCRConfig config,
                                                 Map<String, String> forImage) {
        final Map<String, String> variables = new LinkedHashMap<>(
                config.getOtherTesseractConfig());
        variables.put("page_separator", config.getPageSeparator());
        variables.put("preserve_interword_spaces", config.isPreserveInterwordSpacing() ? "1" : "0");
        variables.putAll(forImage);
        return variables;
    }
}
