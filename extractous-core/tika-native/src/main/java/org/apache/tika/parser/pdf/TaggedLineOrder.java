package org.apache.tika.parser.pdf;

import ai.yobix.PdfConfig;
import ai.yobix.ReadingOrder;
import org.apache.pdfbox.text.PDFTextStripper;
import org.apache.pdfbox.text.TextPosition;

import java.util.List;

/**
 * Tika's writer of tagged PDF text, whose structure tree gives the order of the text, with the
 * glyphs of each line still put in {@link ReadingOrder#orderLines order from the left}, when the
 * {@link PdfConfig} asks for reading order. It lives in Tika's package because the writer's
 * configuration is package-private.
 */
final class TaggedLineOrder {

    private TaggedLineOrder() {
    }

    /** Orders the lines of a page's articles, when `writer` writes tagged text in reading order. */
    static void arrange(PDFTextStripper writer, List<List<TextPosition>> articles) {
        if (writer instanceof PDFMarkedContent2XHTML tagged
                && tagged.config instanceof PdfConfig pdf && pdf.isReadingOrder()) {
            for (final List<TextPosition> article : articles) {
                ReadingOrder.orderLines(article);
            }
        }
    }
}
