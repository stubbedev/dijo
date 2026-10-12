package ai.yobix;

import java.lang.Character.UnicodeScript;
import java.text.Normalizer;
import java.util.EnumSet;
import java.util.Set;

/**
 * Judges the text a PDF page's text layer yields, from signals cheap enough to compute for every
 * page: glyphs that map to no character, characters no real text contains, symbols where letters
 * should be, words mixing alphabets, and Latin letters with too few vowels among them (vowels are
 * a third or more of the letters in any language written in Latin script). Each signal is scaled
 * so that 1 means garbage; the page's badness is the worst of them. Unmapped glyphs count only
 * from their limit on: below it they are as likely lost ligatures of intact text (whole Devanagari
 * or Thai clusters map to their first glyph) as lost text.
 */
final class TextLayerQuality {

    /** A text layer this bad or worse is garbage. */
    static final double GARBAGE = 1.0;
    /** A text layer this bad or worse may read worse than OCR. */
    static final double DOUBTFUL = 0.5;

    private static final double INVALID_SHARE = 0.3;
    private static final double SYMBOL_SHARE = 0.5;
    private static final double MIXED_SCRIPT_SHARE = 0.3;
    private static final double NATURAL_VOWEL_SHARE = 0.32;
    private static final double GARBAGE_VOWEL_SHARE = 0.16;
    private static final int MIN_SAMPLE_CHARACTERS = 20;
    private static final int MIN_SAMPLE_WORDS = 10;
    private static final int MIN_SAMPLE_LATIN_LETTERS = 50;
    private static final Set<UnicodeScript> CASED_ALPHABETS =
            EnumSet.of(UnicodeScript.LATIN, UnicodeScript.GREEK, UnicodeScript.CYRILLIC);
    private static final String SYMBOL_PUNCTUATION = "#%&*@\\_";
    private static final String VOWELS = "aeiouyAEIOUY";

    private TextLayerQuality() {
    }

    /**
     * @param badness 0 for clean text, {@link #GARBAGE} or more for garbage
     * @param fault   the signal that decided the badness, in kebab case
     */
    record Assessment(double badness, String fault) {

        private Assessment worst(double otherBadness, String otherFault) {
            return otherBadness > badness ? new Assessment(otherBadness, otherFault) : this;
        }
    }

    /**
     * @param text          the page's text
     * @param glyphs        the glyphs the page shows
     * @param unmapped      those of them that map to no character
     * @param unmappedLimit the unmapped glyphs that make a page garbage: a share of its glyphs
     *                      below 1, a count from 1
     */
    static Assessment assess(CharSequence text, int glyphs, int unmapped, float unmappedLimit) {
        final Counts counts = count(text);
        if (counts.visible == 0) {
            return new Assessment(GARBAGE, "no-text");
        }
        final double unmappedBadness = unmappedLimit < 1
                ? (double) unmapped / Math.max(glyphs, 1) / unmappedLimit
                : unmapped / (double) unmappedLimit;
        Assessment assessment = new Assessment(
                (double) counts.invalid / counts.visible / INVALID_SHARE, "invalid-characters");
        if (unmappedBadness >= GARBAGE) {
            assessment = assessment.worst(unmappedBadness, "unmapped-glyphs");
        }
        final int lettersDigitsSymbols = counts.letters + counts.digits + counts.symbols;
        if (lettersDigitsSymbols >= MIN_SAMPLE_CHARACTERS) {
            assessment = assessment.worst(
                    (double) counts.symbols / lettersDigitsSymbols / SYMBOL_SHARE, "symbols");
        }
        if (counts.alphabetWords >= MIN_SAMPLE_WORDS) {
            assessment = assessment.worst(
                    (double) counts.mixedScriptWords / counts.alphabetWords / MIXED_SCRIPT_SHARE,
                    "mixed-scripts");
        }
        if (counts.latinLetters >= MIN_SAMPLE_LATIN_LETTERS) {
            final double vowelShare = (double) counts.latinVowels / counts.latinLetters;
            assessment = assessment.worst((NATURAL_VOWEL_SHARE - vowelShare)
                    / (NATURAL_VOWEL_SHARE - GARBAGE_VOWEL_SHARE), "too-few-vowels");
        }
        return assessment;
    }

    private static final class Counts {
        int visible;
        int invalid;
        int letters;
        int digits;
        int symbols;
        int alphabetWords;
        int mixedScriptWords;
        int latinLetters;
        int latinVowels;
    }

    private static Counts count(CharSequence text) {
        final Counts counts = new Counts();
        final StringBuilder word = new StringBuilder();
        for (int index = 0; index < text.length(); ) {
            final int codePoint = Character.codePointAt(text, index);
            index += Character.charCount(codePoint);
            if (isWordCharacter(codePoint)) {
                word.appendCodePoint(codePoint);
            } else {
                countWord(word, counts);
                word.setLength(0);
            }
            if (Character.isWhitespace(codePoint) || Character.isSpaceChar(codePoint)) {
                continue;
            }
            counts.visible++;
            if (isInvalid(codePoint)) {
                counts.invalid++;
            } else if (Character.isLetter(codePoint) || isMark(codePoint)) {
                counts.letters++;
                if (UnicodeScript.of(codePoint) == UnicodeScript.LATIN) {
                    counts.latinLetters++;
                    if (isVowel(codePoint)) {
                        counts.latinVowels++;
                    }
                }
            } else if (Character.isDigit(codePoint)) {
                counts.digits++;
            } else if (isSymbol(codePoint)) {
                counts.symbols++;
            }
        }
        countWord(word, counts);
        return counts;
    }

    private static boolean isWordCharacter(int codePoint) {
        return Character.isLetter(codePoint) || isMark(codePoint);
    }

    private static boolean isMark(int codePoint) {
        final int type = Character.getType(codePoint);
        return type == Character.NON_SPACING_MARK || type == Character.COMBINING_SPACING_MARK
                || type == Character.ENCLOSING_MARK;
    }

    private static boolean isInvalid(int codePoint) {
        if (codePoint == 0xFFFD) {
            return true;
        }
        return switch (Character.getType(codePoint)) {
            case Character.PRIVATE_USE, Character.UNASSIGNED, Character.SURROGATE,
                 Character.CONTROL -> true;
            default -> false;
        };
    }

    private static boolean isSymbol(int codePoint) {
        return switch (Character.getType(codePoint)) {
            case Character.MATH_SYMBOL, Character.CURRENCY_SYMBOL, Character.MODIFIER_SYMBOL,
                 Character.OTHER_SYMBOL -> true;
            default -> SYMBOL_PUNCTUATION.indexOf(codePoint) >= 0;
        };
    }

    private static void countWord(CharSequence word, Counts counts) {
        if (word.isEmpty()) {
            return;
        }
        final Set<UnicodeScript> alphabets = EnumSet.noneOf(UnicodeScript.class);
        int letters = 0;
        for (int index = 0; index < word.length(); ) {
            final int codePoint = Character.codePointAt(word, index);
            index += Character.charCount(codePoint);
            if (!Character.isLetter(codePoint)) {
                continue;
            }
            letters++;
            final UnicodeScript script = UnicodeScript.of(codePoint);
            if (CASED_ALPHABETS.contains(script)) {
                alphabets.add(script);
            }
        }
        if (letters < 2 || alphabets.isEmpty()) {
            return;
        }
        counts.alphabetWords++;
        if (alphabets.size() > 1) {
            counts.mixedScriptWords++;
        }
    }

    private static boolean isVowel(int codePoint) {
        if (codePoint < 0x80) {
            return VOWELS.indexOf(codePoint) >= 0;
        }
        final String decomposed =
                Normalizer.normalize(Character.toString(codePoint), Normalizer.Form.NFD);
        return VOWELS.indexOf(decomposed.charAt(0)) >= 0;
    }
}
