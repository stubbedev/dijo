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
import org.apache.tika.parser.ocr.TesseractOCRParser;
import org.apache.tika.sax.BodyContentHandler;
import org.apache.tika.sax.EmbeddedContentHandler;
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
import java.util.Set;

/**
 * The text recognizer Tika's parsers hand images and rendered pages to: runs OCR on images made
 * readable first, turned upright by their EXIF orientation and by the skew of their text lines,
 * and enlarged when their text is too small. Images that need none of this go to tesseract
 * untouched, as do images too large to decode safely and images Java cannot decode (tesseract
 * reads more formats than ImageIO, WebP and PNM among them). HEIC, HEIF and AVIF images, which neither
 * can decode, are decoded by libheif's heif-dec when it is installed, and reported as unread
 * otherwise rather than passed off as images without text. Each page of a multi-page TIFF is prepared and read on its
 * own, and becomes a page of the output.
 * <p>
 * With the automatic OCR language ({@link OcrConfig}), tesseract's orientation and script
 * detection runs first: the image is also turned by the quarter turns it finds, and read with a
 * model for the script it finds.
 */
final class PreparingOcrParser implements Parser, TextRecognizer {

    private static final long MAX_PIXELS = 60_000_000L;
    private static final Set<String> PAGED_FORMATS = Set.of("tif", "tiff");
    private static final TesseractOCRConfig DEFAULTS = new TesseractOCRConfig();
    private static final Set<MediaType> UNDECODABLE = Set.of(
            MediaType.image("heic"), MediaType.image("heic-sequence"), MediaType.image("heif"),
            MediaType.image("heif-sequence"), MediaType.image("avif"));

    private final TesseractOCRParser ocr;
    private final String tesseract;

    PreparingOcrParser(TesseractOCRParser ocr) {
        this.ocr = ocr;
        this.tesseract = ocr.getDefaultConfig().getTesseractPath()
                + TesseractOCRParser.getTesseractProg();
    }

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        final Set<MediaType> types = new HashSet<>(ocr.getSupportedTypes(context));
        types.add(RecognizedImageParser.WEBP);
        types.addAll(RecognizedImageParser.PNM);
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
                if (pages != null && pages.count() > 1) {
                    parsePages(pages, handler, metadata, context, auto, temporary);
                    return;
                }
                final BufferedImage image = pages == null ? null : pages.read(0);
                final Prepared prepared = prepare(original, image, metadata, auto, temporary);
                prepared.configure(configured);
                metadata.add(OcrReport.KEY, prepared.report(
                        metadata.getInt(TikaPagedText.PAGE_NUMBER), configured));
                try (TikaInputStream preparedInput = prepared.path == null
                        ? null : TikaInputStream.get(prepared.path)) {
                    ocr.parse(preparedInput == null ? input : preparedInput, handler, metadata,
                            context);
                }
            }
        }
    }

    /** Reads every page of a multi-page image as a page of the output. */
    private void parsePages(Pages pages, ContentHandler handler, Metadata metadata,
                            ParseContext context, OcrConfig auto, TemporaryResources temporary)
            throws IOException, SAXException, TikaException {
        final XHTMLContentHandler xhtml = new XHTMLContentHandler(handler, metadata);
        xhtml.startDocument();
        for (int index = 0; index < pages.count(); index++) {
            final BufferedImage image = pages.read(index);
            xhtml.startElement("div", "class", "page");
            if (image == null) {
                metadata.add(TikaCoreProperties.TIKA_META_EXCEPTION_WARNING, "Page " + (index + 1)
                        + " of the image is too large or damaged to OCR");
            } else {
                final Path page = write(image, temporary);
                final Prepared prepared = prepare(page, image, metadata, auto, temporary);
                final TesseractOCRConfig configured = context.get(TesseractOCRConfig.class);
                prepared.configure(configured);
                metadata.add(OcrReport.KEY, prepared.report(index + 1, configured));
                final Metadata pageMetadata = new Metadata();
                pageMetadata.set(TikaPagedText.PAGE_NUMBER, index + 1);
                try (TikaInputStream input = TikaInputStream.get(
                        prepared.path == null ? page : prepared.path)) {
                    ocr.parse(input, new EmbeddedContentHandler(new BodyContentHandler(xhtml)),
                            pageMetadata, context);
                }
            }
            xhtml.endElement("div");
        }
        xhtml.endDocument();
    }

    /**
     * @param path            the prepared image, or null when the original reads as it is
     * @param language        the language to read the image in, when it was detected
     * @param unevenLighting  whether the image is lit unevenly
     * @param script          the script detection found, or null
     * @param exifOrientation the EXIF orientation applied, or null
     * @param decoded         whether Java decoded the image to prepare it
     * @param flattened       whether a page photographed at an angle was made flat
     * @param quarterTurns    the clockwise quarter turns detection found and applied
     * @param skewDegrees     the skew corrected
     * @param scale           the enlargement applied
     */
    private record Prepared(Path path, String language, boolean unevenLighting, String script,
                            String exifOrientation, boolean decoded, boolean flattened,
                            int quarterTurns,
                            double skewDegrees, double scale) {

        String report(Integer page, TesseractOCRConfig configured) {
            final String model = language != null ? language
                    : configured != null ? configured.getLanguage()
                    : DEFAULTS.getLanguage();
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
     * Makes an image readable and, given the configuration of an automatic language, detects the
     * language to read it in.
     *
     * @param original the image file
     * @param image    its decoded pixels, or null when they could not be decoded within limits
     */
    private Prepared prepare(Path original, BufferedImage image, Metadata metadata, OcrConfig auto,
                             TemporaryResources temporary) throws IOException, TikaException {
        final String orientation = metadata.get(TIFF.ORIENTATION);
        BufferedImage upright = image == null
                ? null : OcrImages.applyOrientation(image, orientation);
        final String exifOrientation = upright != image ? orientation : null;
        final BufferedImage flat = upright == null ? null : PageFlattening.flatten(upright);
        if (flat != null) {
            upright = flat;
        }
        String language = null;
        String script = null;
        int quarterTurns = 0;
        if (auto != null) {
            final Path analysed = upright == null || upright == image
                    ? original : write(upright, temporary);
            final ScriptDetection.Result detected =
                    ScriptDetection.detect(tesseract, analysed, auto.getTimeoutMillis());
            language = ScriptDetection.language(tesseract, detected.script());
            script = detected.script();
            if (upright != null && detected.clockwiseQuarterTurns() != 0) {
                upright = OcrImages.turnClockwise(upright, detected.clockwiseQuarterTurns());
                quarterTurns = detected.clockwiseQuarterTurns();
            }
        }
        if (upright == null) {
            return new Prepared(null, language, false, script, null, false, false, 0, 0, 1);
        }
        final double skew = OcrImages.skewDegrees(upright);
        if (skew != 0) {
            upright = OcrImages.rotate(upright, skew);
        }
        final double upscale = OcrImages.upscaleFactor(upright, MAX_PIXELS);
        if (upscale > 1) {
            upright = OcrImages.scale(upright, upscale);
        }
        return new Prepared(upright == image ? null : write(upright, temporary), language,
                OcrImages.unevenLighting(upright), script, exifOrientation, true, flat != null,
                quarterTurns, skew, Math.max(upscale, 1));
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
