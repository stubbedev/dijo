package ai.yobix;

import org.apache.tika.exception.TikaException;
import org.apache.tika.io.TemporaryResources;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.metadata.TIFF;
import org.apache.tika.mime.MediaType;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.parser.ocr.TesseractOCRConfig;
import org.apache.tika.parser.ocr.TesseractOCRParser;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import javax.imageio.IIOException;
import javax.imageio.ImageIO;
import java.awt.image.BufferedImage;
import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Path;
import java.util.Set;

/**
 * Runs OCR on images turned upright first: by their EXIF orientation, and by the skew of their
 * text lines. Images that need neither go to tesseract untouched, as do images too large to
 * decode safely and images Java cannot decode (tesseract reads more formats than ImageIO).
 * <p>
 * With the automatic OCR language ({@link OcrConfig}), tesseract's orientation and script
 * detection runs first: the image is also turned by the quarter turns it finds, and read with a
 * model for the script it finds.
 */
final class PreparingOcrParser implements Parser {

    private static final long MAX_PIXELS = 60_000_000L;

    private final Parser ocr;
    private final String tesseract;

    PreparingOcrParser(Parser ocr) {
        this.ocr = ocr;
        this.tesseract = ocr instanceof TesseractOCRParser parser
                ? parser.getTesseractPath() + "tesseract"
                : "tesseract";
    }

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return ocr.getSupportedTypes(context);
    }

    @Override
    public void parse(InputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        final TesseractOCRConfig configured = context.get(TesseractOCRConfig.class);
        if (configured != null && configured.isSkipOcr()) {
            ocr.parse(stream, handler, metadata, context);
            return;
        }
        final OcrConfig auto = configured instanceof OcrConfig config && config.isAutoLanguage()
                ? config : null;
        try (TemporaryResources temporary = new TemporaryResources()) {
            final TikaInputStream input = TikaInputStream.get(stream, temporary, metadata);
            final Prepared prepared = prepare(input.getPath(), metadata, auto, temporary);
            if (auto != null) {
                auto.useDetectedLanguage(prepared.language);
            }
            try (TikaInputStream image = prepared.path == null
                    ? null : TikaInputStream.get(prepared.path)) {
                ocr.parse(image == null ? input : image, handler, metadata, context);
            }
        }
    }

    /**
     * @param path     the upright image, or null when the original is already upright
     * @param language the language to read the image in, when it was detected
     */
    private record Prepared(Path path, String language) {
    }

    /**
     * Turns the image upright and, given the configuration of an automatic language, detects the
     * language to read it in.
     */
    private Prepared prepare(Path original, Metadata metadata, OcrConfig auto,
                             TemporaryResources temporary) throws IOException, TikaException {
        final BufferedImage image = readWithinLimit(original);
        BufferedImage upright = image == null
                ? null : OcrImages.applyOrientation(image, metadata.get(TIFF.ORIENTATION));
        String language = null;
        if (auto != null) {
            final Path analysed = upright == null || upright == image
                    ? original : write(upright, temporary);
            final ScriptDetection.Result detected =
                    ScriptDetection.detect(tesseract, analysed, auto.getTimeoutSeconds());
            language = ScriptDetection.language(tesseract, detected.script());
            if (upright != null && detected.clockwiseQuarterTurns() != 0) {
                upright = OcrImages.turnClockwise(upright, detected.clockwiseQuarterTurns());
            }
        }
        if (upright == null) {
            return new Prepared(null, language);
        }
        final double skew = OcrImages.skewDegrees(upright);
        if (skew != 0) {
            upright = OcrImages.rotate(upright, skew);
        }
        return new Prepared(upright == image ? null : write(upright, temporary), language);
    }

    private static Path write(BufferedImage image, TemporaryResources temporary)
            throws IOException {
        final Path path = temporary.createTempFile(".png");
        if (!ImageIO.write(image, "png", path.toFile())) {
            throw new IOException("No PNG writer to prepare an image for OCR");
        }
        return path;
    }

    private static BufferedImage readWithinLimit(Path path) throws IOException {
        try (var input = ImageIO.createImageInputStream(path.toFile())) {
            if (input == null) {
                return null;
            }
            final var readers = ImageIO.getImageReaders(input);
            if (!readers.hasNext()) {
                return null;
            }
            final var reader = readers.next();
            try {
                reader.setInput(input, true, true);
                if ((long) reader.getWidth(0) * reader.getHeight(0) > MAX_PIXELS) {
                    return null;
                }
                return reader.read(0);
            } catch (IIOException e) {
                return null;
            } finally {
                reader.dispose();
            }
        }
    }
}
