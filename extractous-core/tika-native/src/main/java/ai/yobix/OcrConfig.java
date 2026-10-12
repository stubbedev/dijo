package ai.yobix;

import org.apache.tika.parser.ocr.TesseractOCRConfig;

/**
 * Tika's tesseract configuration, plus the language {@value #AUTO_LANGUAGE}: detect each image's
 * script and read it with a model for that script. Tika rejects that value as a language code,
 * so it is kept as a flag here, and PreparingOcrParser sets the detected language per image.
 */
public final class OcrConfig extends TesseractOCRConfig {

    static final String AUTO_LANGUAGE = "auto";

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
}
