package ai.yobix;

import org.apache.pdfbox.pdmodel.font.FontInfo;
import org.apache.pdfbox.pdmodel.font.FontMapper;
import org.apache.pdfbox.pdmodel.font.FontMappers;
import org.apache.pdfbox.pdmodel.font.FontProvider;

import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.util.List;

/**
 * Keeps PDFBox from mapping the fonts a PDF names but does not embed to the fonts installed on
 * the host. It would scan every font file on the system for that, once per user and machine, which
 * takes seconds and on Windows tens of seconds, and the text and rendered pages would then depend
 * on the fonts of the host. With no installed fonts, PDFBox uses the metrics of the standard 14
 * fonts and its bundled fallback font, as it does in the static executable, which has none.
 */
final class PdfFonts {

    private static final FontProvider NO_FONTS = new FontProvider() {
        @Override
        public String toDebugString() {
            return "no installed fonts";
        }

        @Override
        public List<? extends FontInfo> getFontInfo() {
            return List.of();
        }
    };

    private PdfFonts() {
    }

    /** Leaves the host's fonts out of PDFBox's font mapping. */
    static void withoutInstalledFonts() {
        final FontMapper mapper = FontMappers.instance();
        try {
            final Method setProvider = mapper.getClass().getMethod("setProvider",
                    FontProvider.class);
            setProvider.setAccessible(true);
            setProvider.invoke(mapper, NO_FONTS);
        } catch (NoSuchMethodException | IllegalAccessException | InvocationTargetException e) {
            throw new IllegalStateException("Cannot keep PDFBox from scanning the installed fonts",
                    e);
        }
    }
}
