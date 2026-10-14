package org.apache.tika.parser.epub;

import com.oracle.svm.core.annotate.Substitute;
import com.oracle.svm.core.annotate.TargetClass;

/** Tika reads the manifest items outside an EPUB's spine that {@link EpubEmbedded} admits. */
@TargetClass(EpubParser.class)
final class Target_org_apache_tika_parser_epub_EpubParser {

    @Substitute
    private boolean shouldHandleEmbedded(String media) {
        return EpubEmbedded.shouldHandle(media);
    }
}
