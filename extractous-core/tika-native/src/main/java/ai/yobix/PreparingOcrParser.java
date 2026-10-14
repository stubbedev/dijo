package ai.yobix;

import org.apache.tika.exception.TikaException;
import org.apache.tika.exception.UnsupportedFormatException;
import org.apache.tika.io.TemporaryResources;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.HttpHeaders;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.metadata.TIFF;
import org.apache.tika.metadata.TikaCoreProperties;
import org.apache.tika.metadata.TikaPagedText;
import org.apache.tika.mime.MediaType;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.parser.enricher.TextRecognizer;
import org.apache.tika.parser.ocr.TesseractOCRConfig;
import org.apache.tika.sax.XHTMLContentHandler;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import javax.imageio.IIOException;
import javax.imageio.ImageIO;
import javax.imageio.ImageReader;
import javax.imageio.stream.ImageInputStream;
import java.awt.image.BufferedImage;
import java.io.Closeable;
import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Path;
import java.util.HashSet;
import java.util.Iterator;
import java.util.Locale;
import java.util.Objects;
import java.util.Set;
import java.util.stream.Collectors;
import java.util.stream.Stream;

/**
 * The text recognizer Tika's parsers hand images and rendered pages to: runs OCR on images made
 * readable first, turned upright by their EXIF orientation and by the skew of their text lines,
 * and resized so their text lines are of a size tesseract reads well and fast. Images that need
 * none of this go to tesseract
 * untouched, as do images too large to decode safely and images Java cannot decode (tesseract
 * reads more formats than ImageIO, WebP and PNM among them). Photoshop, PCX, TGA and JBIG2
 * images, which tesseract cannot read, always go to it as Java decoded them. HEIC, HEIF and AVIF
 * images, which neither can decode, are decoded by libheif (linked, or its heif-dec when it is
 * installed), and reported as unread otherwise rather than passed off as images without text.
 * Each page of a multi-page TIFF is prepared and read on its own, and becomes a page of the
 * output.
 * <p>
 * An image is first read as it is: with the automatic OCR language ({@link OcrConfig}), as
 * upright Latin text. When tesseract is not confident of that reading, its orientation and script
 * detection runs: the image is turned by the quarter turns it finds and, with the automatic
 * language, read with a model for the script it finds.
 */
final class PreparingOcrParser implements Parser, TextRecognizer {

    private static final long MAX_PIXELS = 60_000_000L;
    private static final Set<String> PAGED_FORMATS = Set.of("tif", "tiff");
    private static final TesseractOCRConfig DEFAULTS = new OcrConfig();
    private static final Set<MediaType> DECODED_FOR_TESSERACT = Stream.concat(
            RecognizedImageParser.DECODED_FOR_TESSERACT.stream(),
            Stream.of(MediaType.image("x-jbig2"))).collect(Collectors.toUnmodifiableSet());
    /**
     * The scripts whose large text reads as well once reduced: the alphabets of Europe. The dots
     * and joins of Arabic, for one, need the resolution.
     */
    private static final Set<String> REDUCIBLE_SCRIPTS = Set.of("Latin", "Cyrillic", "Greek");
    /** The script the first image of an extraction is first read in, without detection. */
    private static final String ASSUMED_SCRIPT = "Latin";
    /**
     * The mean word confidence above which a first reading as upright Latin text stands: upright
     * pages of Latin script read at 90 or more, other scripts and turned pages at 60 or less.
     */
    private static final int MIN_CONFIDENCE = 80;
    private static final Set<MediaType> UNDECODABLE = Set.of(
            MediaType.image("heic"), MediaType.image("heic-sequence"), MediaType.image("heif"),
            MediaType.image("heif-sequence"), MediaType.image("avif"));

    private final Tesseract tesseract;
    private final TesseractParser ocr;

    PreparingOcrParser(Tesseract tesseract) {
        this.tesseract = tesseract;
        this.ocr = new TesseractParser(tesseract);
    }

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        final Set<MediaType> types = new HashSet<>(ocr.getSupportedTypes(context));
        types.addAll(RecognizedImageParser.TYPES);
        types.addAll(DECODED_FOR_TESSERACT);
        types.addAll(UNDECODABLE);
        return types;
    }

    @Override
    public boolean recognizesText(ParseContext context) {
        return ocr.recognizesText(context);
    }

    @Override
    public void parse(TikaInputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        final TesseractOCRConfig configured = context.get(TesseractOCRConfig.class);
        if (configured != null && configured.isSkipOcr()) {
            ocr.parse(stream, handler, metadata, context);
            return;
        }
        final MediaType type = MediaType.parse(metadata.get(HttpHeaders.CONTENT_TYPE));
        final OcrConfig auto = configured instanceof OcrConfig config && config.isAutoLanguage()
                ? config : null;
        try (TemporaryResources temporary = new TemporaryResources()) {
            final TikaInputStream received = TikaInputStream.get(stream, temporary, metadata);
            Path original = received.getPath();
            if (type != null && UNDECODABLE.contains(type.getBaseType())) {
                original = HeifDecoder.decode(original, temporary,
                        (configured != null ? configured : DEFAULTS).getTimeoutMillis());
                if (original == null) {
                    metadata.add(TikaCoreProperties.TIKA_META_EXCEPTION_WARNING,
                            new UnsupportedFormatException("The " + type.getBaseType()
                                    + " image was not read for text: decoding it for OCR needs "
                                    + "libheif's heif-dec on the PATH").toString());
                    return;
                }
            }
            final TikaInputStream input;
            if (original.equals(received.getPath())) {
                input = received;
            } else {
                input = TikaInputStream.get(original);
                temporary.addResource(input);
            }
            try (Pages pages = Pages.open(original)) {
                final XHTMLContentHandler xhtml =
                        new XHTMLContentHandler(handler, metadata, context);
                xhtml.startDocument();
                if (pages != null && pages.count() > 1) {
                    readPages(pages, xhtml, metadata, configured, auto, temporary);
                } else {
                    final BufferedImage image = pages == null ? null : pages.read(0);
                    read(xhtml, readableByTesseract(original, type, image, temporary), image,
                            metadata, metadata.getInt(TikaPagedText.PAGE_NUMBER), configured,
                            auto, temporary);
                }
                xhtml.endDocument();
            }
        }
    }

    /** Reads every page of a multi-page image as a page of the output. */
    private void readPages(Pages pages, XHTMLContentHandler xhtml, Metadata metadata,
                           TesseractOCRConfig configured, OcrConfig auto,
                           TemporaryResources temporary)
            throws IOException, SAXException, TikaException {
        for (int index = 0; index < pages.count(); index++) {
            final BufferedImage image = pages.read(index);
            xhtml.startElement("div", "class", "page");
            if (image == null) {
                metadata.add(TikaCoreProperties.TIKA_META_EXCEPTION_WARNING, "Page " + (index + 1)
                        + " of the image is too large or damaged to OCR");
            } else {
                read(xhtml, write(image, temporary), image, metadata, index + 1, configured, auto,
                        temporary);
            }
            xhtml.endElement("div");
        }
    }

    /**
     * Prepares an image, reads it into the output and adds its OCR report. The image is first
     * read as it is, upright text with the automatic language in the script detection last found
     * in this extraction, or else in Latin script, the most common kind, or in the configured
     * language otherwise. Only when tesseract is not confident of that
     * reading ({@value #MIN_CONFIDENCE} or less) does its orientation and script detection run;
     * the image is then turned by the quarter turns it finds and, with the automatic language,
     * read with a model for the script it finds, unless that would read it as before.
     *
     * @param file  the image as tesseract can read it
     * @param image its decoded pixels, or null when they could not be decoded within limits
     * @param page  the page number of the image, when it is a page
     */
    private void read(XHTMLContentHandler xhtml, Path file, BufferedImage image, Metadata metadata,
                      Integer page, TesseractOCRConfig configured, OcrConfig auto,
                      TemporaryResources temporary)
            throws IOException, SAXException, TikaException {
        final TesseractOCRConfig config = configured != null ? configured : DEFAULTS;
        final Oriented oriented = Oriented.of(image, metadata);
        final String assumedScript = auto == null ? null
                : auto.assumedScript() != null ? auto.assumedScript() : ASSUMED_SCRIPT;
        final Prepared first = finish(file, image, oriented, assumedScript,
                language(assumedScript, auto), 0, temporary);
        final Tesseract.Reading reading = read(file, first, config);
        if (reading == null || reading.confidence() > MIN_CONFIDENCE) {
            emit(xhtml, metadata, page, first, reading, config);
            return;
        }
        final Path analysed = oriented.image() == null || oriented.image() == image
                ? file : write(oriented.image(), temporary);
        final ScriptDetection.Result detected =
                ScriptDetection.detect(tesseract, analysed, config.getTimeoutMillis());
        final String script = auto != null ? detected.script() : null;
        final String language = language(script, auto);
        if (auto != null && script != null) {
            auto.assumeScript(script);
        }
        if (detected.clockwiseQuarterTurns() == 0 && Objects.equals(language, first.language())) {
            emit(xhtml, metadata, page, first, reading, config);
            return;
        }
        final Prepared detectedPrepared = finish(file, image, oriented, script, language,
                detected.clockwiseQuarterTurns(), temporary);
        emit(xhtml, metadata, page, detectedPrepared, read(file, detectedPrepared, config),
                config);
    }

    /**
     * The language to read a script in with the automatic language, or null to read in the
     * configured one.
     */
    private String language(String script, OcrConfig auto) throws IOException {
        return auto != null ? ScriptDetection.language(tesseract, script) : null;
    }

    private Tesseract.Reading read(Path file, Prepared prepared, TesseractOCRConfig config)
            throws IOException, TikaException {
        prepared.configure(config);
        return ocr.read(prepared.path == null ? file : prepared.path, config);
    }

    private static void emit(XHTMLContentHandler xhtml, Metadata metadata, Integer page,
                             Prepared prepared, Tesseract.Reading reading,
                             TesseractOCRConfig config) throws SAXException {
        metadata.add(OcrReport.KEY, prepared.report(page, config));
        if (reading != null) {
            TesseractParser.write(xhtml, reading.text());
        }
    }

    /**
     * @param path            the prepared image, or null when the original reads as it is
     * @param language        the language to read the image in, when it was chosen for its
     *                        script
     * @param unevenLighting  whether the image is lit unevenly
     * @param script          the script the image is read in: the one detection found, or the
     *                        one assumed for a first reading, or null
     * @param exifOrientation the EXIF orientation applied, or null
     * @param decoded         whether Java decoded the image to prepare it
     * @param flattened       whether a page photographed at an angle was made flat
     * @param quarterTurns    the clockwise quarter turns detection found and applied
     * @param skewDegrees     the skew corrected
     * @param scale           the resize applied, above 1 to enlarge small text
     */
    private record Prepared(Path path, String language, boolean unevenLighting, String script,
                            String exifOrientation, boolean decoded, boolean flattened,
                            int quarterTurns,
                            double skewDegrees, double scale) {

        String report(Integer page, TesseractOCRConfig configured) {
            final String model = language != null ? language : configured.getLanguage();
            return OcrReport.of(model)
                    .page(page)
                    .script(script)
                    .exifOrientation(exifOrientation)
                    .prepared(decoded, flattened, quarterTurns, skewDegrees, scale, unevenLighting)
                    .json();
        }

        void configure(TesseractOCRConfig config) {
            if (config instanceof OcrConfig ocrConfig) {
                if (ocrConfig.isAutoLanguage()) {
                    ocrConfig.useDetectedLanguage(language);
                }
                ocrConfig.useThresholdingFor(unevenLighting);
            }
        }
    }

    /**
     * An image turned upright by its EXIF orientation and, when photographed at an angle, made
     * flat: the first steps of preparing it, which come before orientation detection.
     *
     * @param image           the upright image, or null when the image could not be decoded
     * @param exifOrientation the EXIF orientation applied, or null
     * @param flattened       whether it was made flat
     */
    private record Oriented(BufferedImage image, String exifOrientation, boolean flattened) {

        static Oriented of(BufferedImage image, Metadata metadata) {
            if (image == null) {
                return new Oriented(null, null, false);
            }
            final String orientation = metadata.get(TIFF.ORIENTATION);
            final BufferedImage upright = OcrImages.applyOrientation(image, orientation);
            final BufferedImage flat = PageFlattening.flatten(upright);
            return new Oriented(flat != null ? flat : upright,
                    upright != image ? orientation : null, flat != null);
        }
    }

    /**
     * Turns an oriented image by the quarter turns detection found, corrects its skew, scales its
     * text to a readable size and checks its lighting.
     */
    private static Prepared finish(Path file, BufferedImage image, Oriented oriented,
                                   String script, String language, int quarterTurns,
                                   TemporaryResources temporary) throws IOException {
        if (oriented.image() == null) {
            return new Prepared(null, language, false, script, null, false, false, 0, 0, 1);
        }
        BufferedImage upright = quarterTurns == 0
                ? oriented.image() : OcrImages.turnClockwise(oriented.image(), quarterTurns);
        final double skew = OcrImages.skewDegrees(upright);
        if (skew != 0) {
            upright = OcrImages.rotate(upright, skew);
        }
        final double scale = OcrImages.scaleFactor(upright, MAX_PIXELS,
                script != null && REDUCIBLE_SCRIPTS.contains(script));
        if (scale != 1) {
            upright = OcrImages.scale(upright, scale);
        }
        return new Prepared(upright == image ? null : write(upright, temporary), language,
                OcrImages.unevenLighting(upright), script, oriented.exifOrientation(), true,
                oriented.flattened(), quarterTurns, skew, scale);
    }

    /**
     * The image as a file tesseract reads: the original, or the decoded pixels of an image
     * tesseract cannot read.
     *
     * @throws TikaException when tesseract cannot read the image and Java could not decode it
     */
    private static Path readableByTesseract(Path original, MediaType type, BufferedImage image,
                                            TemporaryResources temporary)
            throws IOException, TikaException {
        if (type == null || !DECODED_FOR_TESSERACT.contains(type.getBaseType())) {
            return original;
        }
        if (image == null) {
            throw new TikaException("The " + type.getBaseType() + " image could not be decoded "
                    + "for OCR: it is damaged or has more than " + MAX_PIXELS + " pixels");
        }
        return write(image, temporary);
    }

    private static Path write(BufferedImage image, TemporaryResources temporary)
            throws IOException {
        final Path path = temporary.createTempFile(".png");
        if (!ImageIO.write(image, "png", path.toFile())) {
            throw new IOException("No PNG writer to prepare an image for OCR");
        }
        return path;
    }

    /**
     * The decodable pages of an image file: all pages of a TIFF, the first image of anything
     * else, since the further frames of a GIF or an icon are not pages.
     */
    private static final class Pages implements Closeable {
        private final ImageInputStream input;
        private final ImageReader reader;
        private final int count;

        private Pages(ImageInputStream input, ImageReader reader, int count) {
            this.input = input;
            this.reader = reader;
            this.count = count;
        }

        /** The pages of the file, or null when Java cannot decode it. */
        static Pages open(Path path) throws IOException {
            final ImageInputStream input = ImageIO.createImageInputStream(path.toFile());
            if (input == null) {
                return null;
            }
            final Iterator<ImageReader> readers = ImageIO.getImageReaders(input);
            if (!readers.hasNext()) {
                input.close();
                return null;
            }
            final ImageReader reader = readers.next();
            try {
                reader.setInput(input, false, true);
                final boolean paged = PAGED_FORMATS.contains(
                        reader.getFormatName().toLowerCase(Locale.ROOT));
                return new Pages(input, reader, paged ? reader.getNumImages(true) : 1);
            } catch (IIOException e) {
                reader.dispose();
                input.close();
                return null;
            }
        }

        int count() {
            return count;
        }

        /** The page's pixels, or null when the page is too large or cannot be decoded. */
        BufferedImage read(int index) throws IOException {
            try {
                if ((long) reader.getWidth(index) * reader.getHeight(index) > MAX_PIXELS) {
                    return null;
                }
                return reader.read(index);
            } catch (IIOException e) {
                return null;
            }
        }

        @Override
        public void close() throws IOException {
            reader.dispose();
            input.close();
        }
    }
}
