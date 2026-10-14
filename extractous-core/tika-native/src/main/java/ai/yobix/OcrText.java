package ai.yobix;

/**
 * Corrections to text tesseract read that need more than the character it read.
 * <ul>
 * <li>Romanian writes s and t with a comma below (ș, ț), Turkish and Gagauz with a cedilla
 * (ş, ţ), and tesseract cannot tell the two marks apart: it reads Romanian scans with cedillas.
 * A reading that also holds an ă, which only Romanian of these writes, gets the commas.</li>
 * <li>Tesseract reads the Greek mu as the micro sign, which Unicode deems the same letter: one
 * beside a Greek letter becomes mu.</li>
 * </ul>
 */
final class OcrText {

    private OcrText() {
    }

    private static final char MICRO_SIGN = '\u00B5';
    private static final char MU = '\u03BC';

    static String corrected(String text) {
        return greekMu(romanianCommas(text));
    }

    private static String romanianCommas(String text) {
        if (text.indexOf('ă') < 0 && text.indexOf('Ă') < 0) {
            return text;
        }
        return text.replace('ş', 'ș').replace('Ş', 'Ș').replace('ţ', 'ț').replace('Ţ', 'Ț');
    }

    private static String greekMu(String text) {
        if (text.indexOf(MICRO_SIGN) < 0) {
            return text;
        }
        final char[] characters = text.toCharArray();
        for (int index = 0; index < characters.length; index++) {
            if (characters[index] == MICRO_SIGN
                    && (isGreek(characters, index - 1) || isGreek(characters, index + 1))) {
                characters[index] = MU;
            }
        }
        return new String(characters);
    }

    private static boolean isGreek(char[] characters, int index) {
        return index >= 0 && index < characters.length && Character.isLetter(characters[index])
                && Character.UnicodeScript.of(characters[index]) == Character.UnicodeScript.GREEK;
    }
}
