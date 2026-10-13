package ai.yobix;

import java.io.IOException;
import java.nio.file.Path;
import java.util.Map;
import java.util.Set;

/**
 * Tesseract's orientation and script detection (OSD), and the choice of a language model for
 * the script it finds among the models that are installed.
 */
final class ScriptDetection {

    /** The script OSD found, or null when it found none, and the turns that make it upright. */
    record Result(String script, int clockwiseQuarterTurns) {
    }

    private static final Result UNKNOWN = new Result(null, 0);
    private static final String DEFAULT_LANGUAGE = "eng";
    private static final String SCRIPT_MODEL_PREFIX = "script/";

    private static final Map<String, String> LANGUAGE_BY_SCRIPT = Map.ofEntries(
            Map.entry("Latin", "eng"),
            Map.entry("Arabic", "ara"),
            Map.entry("Cyrillic", "rus"),
            Map.entry("Greek", "ell"),
            Map.entry("Hebrew", "heb"),
            Map.entry("Han", "chi_sim"),
            Map.entry("Japanese", "jpn"),
            Map.entry("Hangul", "kor"),
            Map.entry("Thai", "tha"),
            Map.entry("Devanagari", "hin"),
            Map.entry("Bengali", "ben"),
            Map.entry("Tamil", "tam"),
            Map.entry("Telugu", "tel"),
            Map.entry("Kannada", "kan"),
            Map.entry("Malayalam", "mal"),
            Map.entry("Gujarati", "guj"),
            Map.entry("Gurmukhi", "pan"),
            Map.entry("Georgian", "kat"),
            Map.entry("Armenian", "hye"),
            Map.entry("Ethiopic", "amh"),
            Map.entry("Khmer", "khm"),
            Map.entry("Lao", "lao"),
            Map.entry("Myanmar", "mya"),
            Map.entry("Sinhala", "sin"),
            Map.entry("Tibetan", "bod"));

    private ScriptDetection() {
    }

    /**
     * Runs OSD on an image. An image OSD cannot judge, such as one with too little text, gives
     * an unknown script and no turns.
     */
    static Result detect(Tesseract tesseract, Path image, long timeoutMillis) throws IOException {
        final String output = tesseract.detect(image, timeoutMillis);
        if (output == null) {
            return UNKNOWN;
        }
        String script = null;
        int degrees = 0;
        for (final String line : output.lines().toList()) {
            final int colon = line.indexOf(':');
            if (colon < 0) {
                continue;
            }
            final String value = line.substring(colon + 1).trim();
            switch (line.substring(0, colon).trim()) {
                case "Script" -> script = value;
                case "Rotate" -> degrees = Integer.parseInt(value);
                default -> {
                }
            }
        }
        return new Result(script, Math.floorMod(degrees / 90, 4));
    }

    /**
     * The model to read a script with: the script model, then the script's most common
     * language, if installed, and English otherwise.
     */
    static String language(Tesseract tesseract, String script) throws IOException {
        if (script == null) {
            return DEFAULT_LANGUAGE;
        }
        final Set<String> models = tesseract.languages();
        if (models.contains(SCRIPT_MODEL_PREFIX + script)) {
            return SCRIPT_MODEL_PREFIX + script;
        }
        final String language = LANGUAGE_BY_SCRIPT.get(script);
        return language != null && models.contains(language) ? language : DEFAULT_LANGUAGE;
    }
}
