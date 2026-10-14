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
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.HashSet;
import java.util.Iterator;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Objects;
import java.util.Optional;
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
    /**
     * Scripts tesseract's detection takes for others (Armenian for upside-down Latin, Georgian
     * for Malayalam), tried in turn when no reading of an image with plenty of text is confident.
     */
    private static final List<String> OVERLOOKED_SCRIPTS = List.of("Armenian", "Georgian");
    private static final int MIN_WORDS_TO_RETRY = 20;
    /**
     * The first words of a reading in the assumed script that show the page is in another one:
     * the Latin model reads the first twenty words of a Cyrillic, Greek, Armenian or Georgian
     * page with a mean confidence of at most about 55, and of a Latin page with at least about
     * 88.
     */
    private static final int POOR_AFTER_WORDS = 20;
    private static final int POOR_CONFIDENCE = 70;
    private static final Tesseract.Poor POOR = new Tesseract.Poor(POOR_AFTER_WORDS,
            POOR_CONFIDENCE);
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
        final AssumedScript fixed = context.get(AssumedScript.class);
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
                    readPages(pages, Files.size(original), xhtml, metadata, configured, auto,
                            fixed);
                } else {
                    final BufferedImage image = pages == null ? null : pages.read(0);
                    final Recognized read = recognize(readableByTesseract(original, type, image),
                            Files.size(original), image, metadata.get(TIFF.ORIENTATION),
                            configured, auto, assumed(auto, fixed));
                    learn(read.learned(), auto, fixed, metadata);
                    emit(xhtml, metadata, metadata.getInt(TikaPagedText.PAGE_NUMBER), read.read(),
                            configured != null ? configured : DEFAULTS);
                }
                xhtml.endDocument();
            }
        }
    }

    /**
     * Reads every page of a multi-page image as a page of the output, decoded here in turn: the
     * first as any image is read, the rest side by side on {@link PageReadings}, each first read
     * in the script the first page left the extraction assuming, and passed on in order.
     */
    private void readPages(Pages pages, long size, XHTMLContentHandler xhtml, Metadata metadata,
                           TesseractOCRConfig configured, OcrConfig auto, AssumedScript fixed)
            throws IOException, SAXException, TikaException {
        final TesseractOCRConfig config = configured != null ? configured : DEFAULTS;
        final String orientation = metadata.get(TIFF.ORIENTATION);
        final PageReadings.InOrder outputs = new PageReadings.InOrder();
        String rest = null;
        try {
            for (int index = 0; index < pages.count(); index++) {
                final int page = index + 1;
                final BufferedImage image = pages.read(index);
                if (image == null) {
                    outputs.ready(() -> {
                        xhtml.startElement("div", "class", "page");
                        metadata.add(TikaCoreProperties.TIKA_META_EXCEPTION_WARNING, "Page "
                                + page + " of the image is too large or damaged to OCR");
                        xhtml.endElement("div");
                    });
                    continue;
                }
                final OcrImage pixels = new OcrImage.Pixels(image);
                if (index == 0) {
                    final String assumed = assumed(auto, fixed);
                    final Recognized read = recognize(pixels, size, image, orientation,
                            configured, auto, assumed);
                    learn(read.learned(), auto, fixed, metadata);
                    rest = read.learned() != null ? read.learned() : assumed;
                    outputs.ready(page(xhtml, metadata, page, read.read(), config));
                    continue;
                }
                final String assumed = rest;
                outputs.read(() -> page(xhtml, metadata, page, recognize(pixels, size, image,
                        orientation, configured, auto, assumed).read(), config));
            }
            outputs.passAll();
        } finally {
            outputs.abandon();
        }
    }

    private static PageReadings.Output page(XHTMLContentHandler xhtml, Metadata metadata,
                                            int page, Attempt read, TesseractOCRConfig config) {
        return () -> {
            xhtml.startElement("div", "class", "page");
            emit(xhtml, metadata, page, read, config);
            xhtml.endElement("div");
        };
    }

    /**
     * The script an image is first read in with the automatic language: the page's own when its
     * pages are read side by side, or else the one the extraction last learned.
     */
    private static String assumed(OcrConfig auto, AssumedScript fixed) {
        return auto == null ? null : fixed != null ? fixed.script() : auto.assumedScript();
    }

    /**
     * Keeps a script an image's reading learned: in the extraction's memory, or for a page read
     * side by side with others, in its metadata for whoever reads the pages.
     */
    private static void learn(String learned, OcrConfig auto, AssumedScript fixed,
                              Metadata metadata) {
        if (auto == null || learned == null) {
            return;
        }
        if (fixed != null) {
            metadata.set(AssumedScript.LEARNED, learned);
        } else {
            auto.assumeScript(learned);
        }
    }

    /** How an image was read, and the script it taught, if any. */
    private record Recognized(Attempt read, String learned) {
    }

    /**
     * Prepares an image and reads it: what is read, how, and the script the extraction learns
     * from it, if any. The image is first read as it is, upright text with the automatic language
     * in the script assumed, or in the configured language otherwise. Only when tesseract is not
     * confident of that reading ({@value #MIN_CONFIDENCE} or less) does its orientation and
     * script detection run; the image is then turned by the quarter turns it finds and, with the
     * automatic language, read with a model for the script it finds, unless that would read it
     * as before. When no reading of an image with at least {@value #MIN_WORDS_TO_RETRY} words is
     * confident, the scripts detection overlooks are tried as well, and the most confident reading
     * is kept. With the automatic language, each of these readings is a guess, given up once its
     * first {@value #POOR_AFTER_WORDS} words are read with a mean confidence below
     * {@value #POOR_CONFIDENCE}, as a page in another script or turned is; when every one is
     * given up, the image is read to the end as detection found it, in the first reading's script
     * when detection found none. Without a reading when the image is outside the configured
     * limits.
     *
     * @param file        the image as tesseract can read it
     * @param size        the size of the image's file, for the configured limits
     * @param image       its decoded pixels, or null when they could not be decoded within limits
     * @param orientation the image's EXIF orientation, or null
     * @param assumed     the script to read it in first with the automatic language: the one
     *                    detection last found in this extraction, or null for Latin, the most
     *                    common kind
     */
    private Recognized recognize(OcrImage file, long size, BufferedImage image,
                                 String orientation, TesseractOCRConfig configured,
                                 OcrConfig auto, String assumed)
            throws IOException, TikaException {
        final TesseractOCRConfig config = configured != null ? configured : DEFAULTS;
        final Oriented oriented = Oriented.of(image, orientation);
        final String assumedScript = auto == null ? null
                : assumed != null ? assumed : ASSUMED_SCRIPT;
        final Prepared first = finish(image, oriented, assumedScript,
                language(assumedScript, auto), 0);
        if (!TesseractParser.withinLimits(size, config)) {
            return new Recognized(new Attempt(first, null), null);
        }
        final Optional<Tesseract.Reading> optimistic = auto == null
                ? Optional.of(read(file, first, config))
                : readUnlessPoor(file, first, config);
        if (optimistic.isPresent() && optimistic.get().confidence() > MIN_CONFIDENCE) {
            return new Recognized(new Attempt(first, optimistic.get()), null);
        }
        final ScriptDetection.Result detected = ScriptDetection.detect(tesseract,
                first.image() != null ? first.image() : file, config.getTimeoutMillis());
        final String script = auto == null ? null
                : detected.script() == null && optimistic.isEmpty() ? first.script()
                : detected.script();
        final String language = language(script, auto);
        String learned = auto != null ? script : null;
        Attempt best = new Attempt(first, optimistic.orElse(null));
        Prepared fallback = first;
        if (detected.clockwiseQuarterTurns() != 0 || !Objects.equals(language, first.language())) {
            fallback = prepare(first, image, oriented, script, language,
                    detected.clockwiseQuarterTurns());
            final Optional<Tesseract.Reading> reading = auto != null
                    ? readUnlessPoor(file, fallback, config)
                    : Optional.of(read(file, fallback, config));
            if (reading.isPresent()) {
                best = best.or(new Attempt(fallback, reading.get()));
            }
        }
        if (auto != null && !best.confident()
                && (best.reading() == null || best.words() >= MIN_WORDS_TO_RETRY)) {
            best = readOverlooked(best, file, image, oriented, config);
            if (best.prepared().script() != null) {
                learned = best.prepared().script();
            }
        }
        if (best.reading() == null) {
            best = new Attempt(fallback, read(file, fallback, config));
        }
        return new Recognized(best, learned);
    }

    /**
     * Reads an image in the scripts detection overlooks, until one reads it confidently, giving
     * up each reading that goes poorly: the best reading, whose script the rest of the
     * extraction then assumes.
     */
    private Attempt readOverlooked(Attempt best, OcrImage file, BufferedImage image,
                                   Oriented oriented, TesseractOCRConfig config)
            throws IOException, TikaException {
        Attempt overlooked = best;
        for (final String script : OVERLOOKED_SCRIPTS) {
            final String model = ScriptDetection.installedLanguage(tesseract, script);
            if (model == null || overlooked.confident()) {
                continue;
            }
            final Prepared retried = prepare(best.prepared(), image, oriented, script, model, 0);
            final Optional<Tesseract.Reading> reading = readUnlessPoor(file, retried, config);
            if (reading.isPresent()) {
                overlooked = overlooked.or(new Attempt(retried, reading.get()));
            }
        }
        return overlooked;
    }

    /** A way an image was prepared and what tesseract read in it that way. */
    private record Attempt(Prepared prepared, Tesseract.Reading reading) {

        boolean confident() {
            return reading != null && reading.confidence() > MIN_CONFIDENCE;
        }

        int words() {
            return reading == null ? 0 : reading.text().split("\\s+").length;
        }

        /** The more confident of this attempt and another, this one on a tie. */
        Attempt or(Attempt other) {
            final int confidence = reading == null ? -1 : reading.confidence();
            final int otherConfidence = other.reading == null ? -1 : other.reading.confidence();
            return otherConfidence > confidence ? other : this;
        }
    }

    /**
     * The language to read a script in with the automatic language, or null to read in the
     * configured one.
     */
    private String language(String script, OcrConfig auto) throws IOException {
        return auto != null ? ScriptDetection.language(tesseract, script) : null;
    }

    private Tesseract.Reading read(OcrImage file, Prepared prepared, TesseractOCRConfig config)
            throws IOException, TikaException {
        return ocr.read(prepared.image == null ? file : prepared.image, config,
                prepared.language(config), prepared.variables(config));
    }

    private Optional<Tesseract.Reading> readUnlessPoor(OcrImage file, Prepared prepared,
                                                       TesseractOCRConfig config)
            throws IOException, TikaException {
        return ocr.readUnlessPoor(prepared.image == null ? file : prepared.image, config,
                prepared.language(config), prepared.variables(config), POOR);
    }

    private static void emit(XHTMLContentHandler xhtml, Metadata metadata, Integer page,
                             Attempt read, TesseractOCRConfig config) throws SAXException {
        metadata.add(OcrReport.KEY, read.prepared().report(page, config));
        if (read.reading() != null) {
            TesseractParser.write(xhtml, read.reading().text());
        }
    }

    /**
     * @param image           the prepared image, or null when the original reads as it is
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
    private record Prepared(OcrImage image, String language, boolean unevenLighting, String script,
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

        /** This preparation, to read in another script and language. */
        Prepared readIn(String otherScript, String otherLanguage) {
            return new Prepared(image, otherLanguage, unevenLighting, otherScript, exifOrientation,
                    decoded, flattened, quarterTurns, skewDegrees, scale);
        }

        /** The language to read the image in: its own with the automatic language. */
        String language(TesseractOCRConfig config) {
            return config instanceof OcrConfig ocrConfig && ocrConfig.isAutoLanguage()
                    ? language : config.getLanguage();
        }

        /** The variables to read the image with beyond the configured ones. */
        Map<String, String> variables(TesseractOCRConfig config) {
            return config instanceof OcrConfig ? OcrConfig.thresholdingFor(unevenLighting)
                    : Map.of();
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

        static Oriented of(BufferedImage image, String orientation) {
            if (image == null) {
                return new Oriented(null, null, false);
            }
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
    /**
     * Prepares an image to read in a script and language as {@link #finish} does, reusing an
     * earlier preparation that turned it as far and reduces it as this script is reduced.
     */
    private static Prepared prepare(Prepared earlier, BufferedImage image, Oriented oriented,
                                    String script, String language, int quarterTurns) {
        if (quarterTurns == earlier.quarterTurns()
                && reducible(script) == reducible(earlier.script())) {
            return earlier.readIn(script, language);
        }
        return finish(image, oriented, script, language, quarterTurns);
    }

    private static boolean reducible(String script) {
        return script != null && REDUCIBLE_SCRIPTS.contains(script);
    }

    private static Prepared finish(BufferedImage image, Oriented oriented, String script,
                                   String language, int quarterTurns) {
        if (oriented.image() == null) {
            return new Prepared(null, language, false, script, null, false, false, 0, 0, 1);
        }
        BufferedImage upright = quarterTurns == 0
                ? oriented.image() : OcrImages.turnClockwise(oriented.image(), quarterTurns);
        final double skew = OcrImages.skewDegrees(upright);
        if (skew != 0) {
            upright = OcrImages.rotate(upright, skew);
        }
        final double scale = OcrImages.scaleFactor(upright, MAX_PIXELS, reducible(script));
        if (scale != 1) {
            upright = OcrImages.scale(upright, scale);
        }
        return new Prepared(upright == image ? null : new OcrImage.Pixels(upright), language,
                OcrImages.unevenLighting(upright), script, oriented.exifOrientation(), true,
                oriented.flattened(), quarterTurns, skew, scale);
    }

    /**
     * The image as tesseract reads it: the original file, or the decoded pixels of an image
     * tesseract cannot decode.
     *
     * @throws TikaException when tesseract cannot decode the image and Java could not either
     */
    private static OcrImage readableByTesseract(Path original, MediaType type,
                                                BufferedImage image) throws TikaException {
        if (type == null || !DECODED_FOR_TESSERACT.contains(type.getBaseType())) {
            return new OcrImage.File(original);
        }
        if (image == null) {
            throw new TikaException("The " + type.getBaseType() + " image could not be decoded "
                    + "for OCR: it is damaged or has more than " + MAX_PIXELS + " pixels");
        }
        return new OcrImage.Pixels(image);
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
