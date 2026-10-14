package ai.yobix;

import java.io.IOException;
import java.nio.file.Path;

/**
 * The tesseract and language data the build bundled, which the Rust side unpacks and names in
 * system properties: {@code extractous.ocr.tessdata} names the language data, and
 * {@code extractous.ocr.tesseract} the executable, unless tesseract is linked into the static
 * executable.
 */
final class OcrBundle {

    private static final String TESSERACT = System.getProperty("extractous.ocr.tesseract");
    private static final String TESSDATA = System.getProperty("extractous.ocr.tessdata");

    private OcrBundle() {
    }

    /**
     * The bundled tesseract, or null when the build bundled none.
     *
     * @throws IllegalStateException when the bundled executable does not run
     */
    static Tesseract tesseract() throws IOException {
        if (StaticImage.running()) {
            if (TESSDATA == null) {
                throw new IllegalStateException("The static executable names no language data for "
                        + "its linked tesseract");
            }
            return new LinkedTesseract(Path.of(TESSDATA));
        }
        if (TESSERACT == null) {
            return null;
        }
        final TesseractProgram program = new TesseractProgram(TESSERACT, TESSDATA);
        if (!program.runs()) {
            throw new IllegalStateException("The bundled tesseract does not run");
        }
        return program;
    }
}
