package ai.yobix;

import java.io.IOException;
import java.nio.file.Path;

/**
 * The tesseract and language data the build bundled, which the Rust side unpacks and names in
 * system properties: {@code extractous.ocr.tessdata} names the language data, and
 * {@code extractous.ocr.heif} the heif-png that decodes HEIC, HEIF and AVIF, on the platforms
 * whose bundle carries one.
 */
final class OcrBundle {

    private static final String TESSDATA = System.getProperty("extractous.ocr.tessdata");
    private static final String HEIF_DECODER = System.getProperty("extractous.ocr.heif");

    private OcrBundle() {
    }

    /** The bundled heif-png, or null when the build bundled none. */
    static Path heifDecoder() {
        return HEIF_DECODER == null ? null : Path.of(HEIF_DECODER);
    }

    /**
     * The tesseract linked into the static executable with the bundled language data, or null
     * outside it: the library other builds bundle carries only tesseract's C API, which native
     * readings call.
     */
    static Tesseract tesseract() throws IOException {
        if (!StaticImage.running()) {
            return null;
        }
        if (TESSDATA == null) {
            throw new IllegalStateException("The build names no language data for its tesseract");
        }
        return new LinkedTesseract(Path.of(TESSDATA));
    }
}
