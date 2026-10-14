package ai.yobix;

import org.apache.fontbox.FontBoxFont;
import org.apache.fontbox.ttf.TTFParser;
import org.apache.fontbox.ttf.TrueTypeFont;
import org.apache.pdfbox.io.RandomAccessReadBuffer;
import org.apache.pdfbox.pdmodel.font.CIDFontMapping;
import org.apache.pdfbox.pdmodel.font.FontMapper;
import org.apache.pdfbox.pdmodel.font.FontMapping;
import org.apache.pdfbox.pdmodel.font.FontMappers;
import org.apache.pdfbox.pdmodel.font.PDCIDSystemInfo;
import org.apache.pdfbox.pdmodel.font.PDFontDescriptor;

import java.io.IOException;
import java.io.InputStream;
import java.io.UncheckedIOException;

/**
 * The fonts PDFBox uses for those a PDF names but does not embed: always the fallback font it
 * bundles, which is what its own mapper answers on a host with no fonts installed. Its own mapper
 * would first scan every font file installed on the host, once per user and machine, which takes
 * seconds and on Windows minutes, and the text and rendered pages would then depend on the fonts
 * of the host.
 */
final class PdfFonts implements FontMapper {

    private static final String FALLBACK_FONT =
            "/org/apache/pdfbox/resources/ttf/LiberationSans-Regular.ttf";

    private PdfFonts() {
    }

    /** Makes PDFBox map every font it does not find embedded to its fallback font. */
    static void install() {
        FontMappers.set(new PdfFonts());
    }

    @Override
    public FontMapping<TrueTypeFont> getTrueTypeFont(String baseFont,
                                                     PDFontDescriptor fontDescriptor) {
        return new FontMapping<>(Fallback.FONT, true);
    }

    @Override
    public FontMapping<FontBoxFont> getFontBoxFont(String baseFont,
                                                   PDFontDescriptor fontDescriptor) {
        return new FontMapping<>(Fallback.FONT, true);
    }

    @Override
    public CIDFontMapping getCIDFont(String baseFont, PDFontDescriptor fontDescriptor,
                                     PDCIDSystemInfo cidSystemInfo) {
        return new CIDFontMapping(null, Fallback.FONT, true);
    }

    private static final class Fallback {

        static final TrueTypeFont FONT = load();

        private static TrueTypeFont load() {
            try (InputStream font = FontMapper.class.getResourceAsStream(FALLBACK_FONT)) {
                if (font == null) {
                    throw new IllegalStateException("PDFBox has no " + FALLBACK_FONT);
                }
                return new TTFParser().parse(RandomAccessReadBuffer.createBufferFromStream(font));
            } catch (IOException e) {
                throw new UncheckedIOException("Cannot read PDFBox's " + FALLBACK_FONT, e);
            }
        }
    }
}
