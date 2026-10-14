package ai.yobix;

import org.apache.tika.parser.ocr.TesseractOCRConfig;

/**
 * Tika's tesseract configuration, plus the language {@value #AUTO_LANGUAGE}: detect each image's
 * script and read it with a model for that script. Tika rejects that value as a language code,
 * so it is kept as a flag here, and PreparingOcrParser sets the detected language per image.
 * Images lit unevenly are binarised with Sauvola's adaptive threshold, which keeps text in shadows
 * that a single threshold for the whole image loses, but thins delicate strokes on clean scans.
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

    void useDetectedLanguage(String language) {
        super.setLanguage(language);
    }

    /** Binarises the next image adaptively when it is lit unevenly. */
    void useThresholdingFor(boolean unevenLighting) {
        addOtherTesseractConfig(THRESHOLDING_METHOD, unevenLighting ? SAUVOLA : OTSU);
    }
}
