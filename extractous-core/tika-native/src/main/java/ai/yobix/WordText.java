package ai.yobix;

/**
 * The text of a Word 97-2003 document as its readers should see it. Word keeps control
 * characters in the text: anchors where a picture, a drawing, a footnote or a comment sits,
 * which are no text of their own; tabs; and breaks of lines, paragraphs, pages and columns.
 * Tika turned all of them into line breaks, cutting a sentence in two at every footnote or
 * comment anchored in it, and left them in the text of comments, footnotes and endnotes, where
 * its XML writer replaced them with U+FFFD.
 */
final class WordText {

    private static final char NON_BREAKING_HYPHEN = '‑';
    private static final char ZERO_WIDTH_SPACE = '​';

    private WordText() {
    }

    static String clean(String text) {
        StringBuilder cleaned = null;
        for (int index = 0; index < text.length(); index++) {
            final char c = text.charAt(index);
            final int mapped = switch (c) {
                case 0x01, 0x02, 0x03, 0x04, 0x05, 0x08 -> -1;
                case '\t' -> '\t';
                case 0x1e -> NON_BREAKING_HYPHEN;
                case 0x1f -> ZERO_WIDTH_SPACE;
                default -> c < 0x20 ? '\n' : c;
            };
            if (cleaned == null && mapped == c) {
                continue;
            }
            if (cleaned == null) {
                cleaned = new StringBuilder(text.length()).append(text, 0, index);
            }
            if (mapped >= 0) {
                cleaned.append((char) mapped);
            }
        }
        return cleaned == null ? text : cleaned.toString();
    }
}
