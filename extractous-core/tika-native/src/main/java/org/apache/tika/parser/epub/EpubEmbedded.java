package org.apache.tika.parser.epub;

import java.util.Locale;
import java.util.Set;

/**
 * Which manifest items outside an EPUB's spine Tika reads as embedded documents: not stylesheets,
 * images in SVG, XML or Apple's display options, as Tika has it, and not the tables of contents,
 * the EPUB 2 NCX and the EPUB 3 navigation document. EPUB 3 lists every content document in the
 * spine, so an XHTML item outside it is the navigation document, which only repeats the headings.
 */
final class EpubEmbedded {

    private static final Set<String> CONTENTS_TYPES =
            Set.of("application/x-dtbncx+xml", "application/xhtml+xml", "text/html");

    private EpubEmbedded() {
    }

    static boolean shouldHandle(String media) {
        if (media == null) {
            return true;
        }
        final String type = media.toLowerCase(Locale.US);
        return !(type.contains("css") || type.contains("svg") || type.endsWith("/xml")
                || type.contains("x-ibooks") || CONTENTS_TYPES.contains(type));
    }
}
