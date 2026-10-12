package ai.yobix;

import org.apache.tika.exception.TikaException;
import org.apache.tika.io.TemporaryResources;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.metadata.TIFF;
import org.apache.tika.mime.MediaType;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
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
 */
final class PreparingOcrParser implements Parser {

    private static final long MAX_PIXELS = 60_000_000L;

    private final Parser ocr;

    PreparingOcrParser(Parser ocr) {
        this.ocr = ocr;
    }

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return ocr.getSupportedTypes(context);
    }

    @Override
    public void parse(InputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        try (TemporaryResources temporary = new TemporaryResources()) {
            final TikaInputStream input = TikaInputStream.get(stream, temporary, metadata);
            final Path prepared = prepare(input.getPath(), metadata, temporary);
            if (prepared == null) {
                ocr.parse(input, handler, metadata, context);
                return;
            }
            try (TikaInputStream upright = TikaInputStream.get(prepared)) {
                ocr.parse(upright, handler, metadata, context);
            }
        }
    }

    /** The upright image written to a temporary file, or null when the original is upright. */
    private static Path prepare(Path original, Metadata metadata, TemporaryResources temporary)
            throws IOException {
        final BufferedImage image = readWithinLimit(original);
        if (image == null) {
            return null;
        }
        final BufferedImage oriented =
                OcrImages.applyOrientation(image, metadata.get(TIFF.ORIENTATION));
        final double skew = OcrImages.skewDegrees(oriented);
        if (oriented == image && skew == 0) {
            return null;
        }
        final BufferedImage upright = skew == 0 ? oriented : OcrImages.rotate(oriented, skew);
        final Path prepared = temporary.createTempFile(".png");
        if (!ImageIO.write(upright, "png", prepared.toFile())) {
            throw new IOException("No PNG writer to prepare an image for OCR");
        }
        return prepared;
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
