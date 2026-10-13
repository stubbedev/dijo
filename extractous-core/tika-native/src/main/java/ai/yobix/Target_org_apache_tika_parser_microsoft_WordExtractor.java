package ai.yobix;

import com.oracle.svm.core.annotate.Substitute;
import com.oracle.svm.core.annotate.TargetClass;

/** Tika writes the runs of Word 97-2003 body text as {@link WordText} cleans them. */
@TargetClass(className = "org.apache.tika.parser.microsoft.WordExtractor")
final class Target_org_apache_tika_parser_microsoft_WordExtractor {

    @Substitute
    private static String cleanControlCharacters(String text) {
        return WordText.clean(text);
    }
}
