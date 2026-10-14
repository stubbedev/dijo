package ai.yobix;

import org.apache.tika.parser.ocr.TesseractOCRConfig;

import java.util.Map;

/**
 * Tika's tesseract configuration, plus the language {@value #AUTO_LANGUAGE}: detect each image's
 * script and read it with a model for that script. Tika rejects that value as a language code,
 * so it is kept as a flag here, and PreparingOcrParser reads each image in the language detected
 * for it. Images lit unevenly are binarised with Sauvola's adaptive threshold
 * ({@link #thresholdingFor}), which keeps text in shadows that a single threshold for the whole
 * image loses, but thins delicate strokes on clean scans.
 * Pages are segmented without tesseract's own orientation and script detection (mode 3, where
 * Tika's default is 1): PreparingOcrParser turns an image upright before reading it, and runs
 * detection only when a first reading needs it.
 */
public final class OcrConfig extends TesseractOCRConfig {

    static final String AUTO_LANGUAGE = "auto";
    private static final String SEGMENTATION_WITHOUT_DETECTION = "3";
    private static final String THRESHOLDING_METHOD = "thresholding_method";
    private static final String OTSU = "0";
    private static final String SAUVOLA = "2";

    private boolean autoLanguage;
    private volatile String assumedScript;

    public OcrConfig() {
        setPageSegMode(SEGMENTATION_WITHOUT_DETECTION);
    }

    @Override
    public void setLanguage(String language) {
        autoLanguage = AUTO_LANGUAGE.equals(language);
        if (!autoLanguage) {
            super.setLanguage(language);
        }
    }

    boolean isAutoLanguage() {
        return autoLanguage;
    }

    /** Sets how long a tesseract run may take, in whole seconds. */
    public void setTimeoutSeconds(int seconds) {
        setTimeoutMillis(seconds * 1000L);
    }

    /**
     * The script the next image of this extraction is first read in, with the automatic
     * language: the one detection last found in an earlier image, or null before any.
     */
    String assumedScript() {
        return assumedScript;
    }

    /** Remembers the script detection found, for the next images of this extraction. */
    void assumeScript(String script) {
        assumedScript = script;
    }

    /** The variable that binarises an image adaptively when it is lit unevenly. */
    static Map<String, String> thresholdingFor(boolean unevenLighting) {
        return Map.of(THRESHOLDING_METHOD, unevenLighting ? SAUVOLA : OTSU);
    }
}
