package ai.yobix;

import org.apache.tika.parser.pages.PagesConfig;
import org.apache.tika.parser.pages.TextPolicy;
import org.apache.tika.parser.pdf.PDFParserConfig;

import java.util.Map;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/**
 * Tika's PDF parser configuration, with the OCR settings extractous sets by name: the strategy
 * and its automatic thresholds, which Tika 4 keeps as the page text policy and OCR thresholds of
 * the configuration's pages overlay.
 */
public final class PdfConfig extends PDFParserConfig {

    private static final Map<String, TextPolicy> STRATEGIES = Map.of(
            "NO_OCR", TextPolicy.EXTRACT,
            "AUTO", TextPolicy.AUTO,
            "OCR_ONLY", TextPolicy.OCR,
            "OCR_AND_TEXT_EXTRACTION", TextPolicy.EXTRACT_AND_OCR);
    private static final Pattern AUTO_THRESHOLDS =
            Pattern.compile("\\s*(?:(better|faster)|(\\d{1,3})(%)?)(?:,\\s*(\\d{1,3}))?\\s*");
    private static final float BETTER_UNMAPPED = 10;
    private static final float FASTER_UNMAPPED = 0.1f;
    private static final int DEFAULT_TOTAL_CHARACTERS = 10;

    /** Sets where page text comes from: NO_OCR, AUTO, OCR_ONLY or OCR_AND_TEXT_EXTRACTION. */
    public void setOcrStrategy(String strategy) {
        final TextPolicy text = STRATEGIES.get(strategy);
        if (text == null) {
            throw new IllegalArgumentException("Unknown OCR strategy " + strategy);
        }
        pages().setText(text);
    }

    /**
     * Sets when the automatic strategy OCRs a page, as Tika 3 took it: "better" (more than 10
     * unmapped glyphs, or at most 10 characters), "faster" (more than a tenth of the glyphs
     * unmapped, or at most 10 characters), or "unmapped[%][,characters]": more unmapped glyphs
     * than a count or a share of the page's glyphs, or at most characters characters (10 when
     * left out).
     */
    public void setOcrStrategyAuto(String thresholds) {
        final PagesConfig.Auto auto = new PagesConfig.Auto();
        final Matcher matcher = AUTO_THRESHOLDS.matcher(thresholds);
        if (!matcher.matches()) {
            throw new IllegalArgumentException("Automatic OCR thresholds are better, faster or "
                    + "unmapped[%][,characters]: " + thresholds);
        }
        final String preset = matcher.group(1);
        auto.setTotalCharsPerPage(matcher.group(4) == null
                ? DEFAULT_TOTAL_CHARACTERS : Integer.parseInt(matcher.group(4)));
        if ("better".equals(preset)) {
            auto.setUnmappedUnicodeCharsPerPage(BETTER_UNMAPPED);
        } else if ("faster".equals(preset)) {
            auto.setUnmappedUnicodeCharsPerPage(FASTER_UNMAPPED);
        } else {
            final float unmapped = Integer.parseInt(matcher.group(2));
            if (matcher.group(3) != null && unmapped > 100) {
                throw new IllegalArgumentException("The share of unmapped glyphs cannot exceed "
                        + "100%: " + thresholds);
            }
            auto.setUnmappedUnicodeCharsPerPage(matcher.group(3) == null ? unmapped : unmapped / 100);
        }
        pages().ocr().setAuto(auto);
    }
}
