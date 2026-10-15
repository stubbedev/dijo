package ai.yobix;

import java.io.IOException;
import java.nio.file.Path;

/**
 * The tesseract and language data the build bundled, which the Rust side unpacks and names in
 * system properties: {@code extractous.ocr.tessdata} names the language data,
 * {@code extractous.ocr.library} the library of {@link LinkedTesseract}'s native functions,
 * unless tesseract is linked into the static executable, and {@code extractous.ocr.heif} the
 * heif-png that decodes HEIC, HEIF and AVIF, on the platforms whose bundle carries one.
 */
final class OcrBundle {

    private static final String LIBRARY = System.getProperty("extractous.ocr.library");
    private static final String TESSDATA = System.getProperty("extractous.ocr.tessdata");
    private static final String HEIF_DECODER = System.getProperty("extractous.ocr.heif");

    private OcrBundle() {
    }

    /** The bundled heif-png, or null when the build bundled none. */
    static Path heifDecoder() {
        return HEIF_DECODER == null ? null : Path.of(HEIF_DECODER);
    }

    /**
     * The bundled tesseract, linked into the static executable or loaded from the bundle's
     * library, or null when the build bundled none.
     */
    static Tesseract tesseract() throws IOException {
        if (!StaticImage.running() && LIBRARY == null) {
            return null;
        }
        if (TESSDATA == null) {
            throw new IllegalStateException("The build names no language data for its tesseract");
        }
        if (LIBRARY != null) {
            System.load(LIBRARY);
        }
        return new LinkedTesseract(Path.of(TESSDATA));
    }
}
