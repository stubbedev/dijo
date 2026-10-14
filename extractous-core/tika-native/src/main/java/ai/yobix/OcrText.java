package ai.yobix;

/**
 * Corrections to text tesseract read that only the whole reading can decide. Romanian writes s
 * and t with a comma below (ș, ț), Turkish and Gagauz with a cedilla (ş, ţ), and tesseract cannot
 * tell the two marks apart: it reads Romanian scans with cedillas. A reading that also holds an ă,
 * which only Romanian of these writes, gets the commas.
 */
final class OcrText {

    private OcrText() {
    }

    static String corrected(String text) {
        if (text.indexOf('ă') < 0 && text.indexOf('Ă') < 0) {
            return text;
        }
        return text.replace('ş', 'ș').replace('Ş', 'Ș').replace('ţ', 'ț').replace('Ţ', 'Ț');
    }
}
