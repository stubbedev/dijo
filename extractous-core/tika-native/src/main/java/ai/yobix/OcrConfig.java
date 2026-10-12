package ai.yobix;

import org.apache.tika.parser.ocr.TesseractOCRConfig;

/**
 * Tika's tesseract configuration, plus the language {@value #AUTO_LANGUAGE}: detect each image's
 * script and read it with a model for that script. Tika rejects that value as a language code,
 * so it is kept as a flag here, and PreparingOcrParser sets the detected language per image.
 * Images lit unevenly are binarised with Sauvola's adaptive threshold, which keeps text in shadows
 * that a single threshold for the whole image loses, but thins delicate strokes on clean scans.
 */
public final class OcrConfig extends TesseractOCRConfig {

    static final String AUTO_LANGUAGE = "auto";
    private static final String THRESHOLDING_METHOD = "thresholding_method";
    private static final String OTSU = "0";
    private static final String SAUVOLA = "2";

    private boolean autoLanguage;

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

    void useDetectedLanguage(String language) {
        super.setLanguage(language);
    }

    /** Binarises the next image adaptively when it is lit unevenly. */
    void useThresholdingFor(boolean unevenLighting) {
        addOtherTesseractConfig(THRESHOLDING_METHOD, unevenLighting ? SAUVOLA : OTSU);
    }
}
