package org.apache.tika.parser.odf;

import com.oracle.svm.core.annotate.Alias;
import com.oracle.svm.core.annotate.Substitute;
import com.oracle.svm.core.annotate.TargetClass;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.microsoft.OfficeParserConfig;

/**
 * An OpenDocument file keeps the text of tracked deletions in its list of tracked changes, ahead
 * of the body. Tika read it as body text; it is now left out like the rest of that list, unless
 * the {@link OfficeParserConfig} asks for deleted content, as it does for Word documents. It
 * lives in Tika's package to share Tika's namespace constants.
 */
@TargetClass(OpenDocumentBodyHandler.class)
final class Target_org_apache_tika_parser_odf_OpenDocumentBodyHandler {

    @Alias
    private ParseContext parseContext;

    @Substitute
    private boolean needsCompleteFiltering(String namespaceURI, String localName) {
        if (OpenDocumentBodyHandler.TEXT_NS.equals(namespaceURI)) {
            if (!"tracked-changes".equals(localName)) {
                return localName.endsWith("-template") || localName.endsWith("-style");
            }
            final OfficeParserConfig office = parseContext.get(OfficeParserConfig.class);
            return office == null || !office.isIncludeDeletedContent();
        }
        return OpenDocumentBodyHandler.TABLE_NS.equals(namespaceURI)
                && "covered-table-cell".equals(localName);
    }
}
