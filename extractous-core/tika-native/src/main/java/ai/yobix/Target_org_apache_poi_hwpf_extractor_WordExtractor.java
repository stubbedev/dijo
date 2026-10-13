package ai.yobix;

import com.oracle.svm.core.annotate.Substitute;
import com.oracle.svm.core.annotate.TargetClass;
import org.apache.poi.hwpf.extractor.WordExtractor;
import org.apache.poi.hwpf.usermodel.Range;

/**
 * POI gives Tika the paragraphs of a Word 97-2003 document's comments, footnotes, endnotes and
 * text boxes with their field codes stripped, keeping the fields' results, and their text as
 * {@link WordText} cleans it, as Tika writes the body.
 */
@TargetClass(WordExtractor.class)
final class Target_org_apache_poi_hwpf_extractor_WordExtractor {

    @Substitute
    static String[] getParagraphText(Range range) {
        final String[] paragraphs = new String[range.numParagraphs()];
        for (int index = 0; index < paragraphs.length; index++) {
            paragraphs[index] =
                    WordText.clean(Range.stripFields(range.getParagraph(index).text()));
        }
        return paragraphs;
    }
}
