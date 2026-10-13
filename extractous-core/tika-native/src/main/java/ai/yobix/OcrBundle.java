package ai.yobix;

import org.apache.tika.exception.TikaConfigException;
import org.apache.tika.parser.ocr.TesseractOCRConfig;
import org.apache.tika.parser.ocr.TesseractOCRParser;

import java.io.File;
import java.util.Map;

/**
 * The tesseract executable and language data the build bundled, which the Rust side unpacks
 * beside the library and names in the {@code extractous.ocr.tesseract} and
 * {@code extractous.ocr.tessdata} properties. Without them, tesseract is looked up on the
 * {@code PATH} with its own language data, as Tika does.
 */
final class OcrBundle {

    private static final String TESSERACT = System.getProperty("extractous.ocr.tesseract");
    private static final String TESSDATA = System.getProperty("extractous.ocr.tessdata");
    private static final String TESSDATA_PREFIX = "TESSDATA_PREFIX";

    private OcrBundle() {
    }

    static boolean present() {
        return TESSERACT != null;
    }

    /** Tika's tesseract parser, running the bundled tesseract with the bundled language data. */
    static TesseractOCRParser parser() throws TikaConfigException {
        final TesseractOCRConfig config = new TesseractOCRConfig();
        config.setTesseractPath(new File(TESSERACT).getParent() + File.separator);
        config.setTessdataPath(TESSDATA);
        return new TesseractOCRParser(config);
    }

    /** Points a tesseract process at the bundled language data, when there is a bundle. */
    static void environment(Map<String, String> environment) {
        if (present()) {
            environment.put(TESSDATA_PREFIX, TESSDATA);
        }
    }
}
