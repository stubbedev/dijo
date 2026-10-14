package org.apache.tika.parser.pdf;

import com.oracle.svm.core.annotate.Alias;
import com.oracle.svm.core.annotate.Substitute;
import com.oracle.svm.core.annotate.TargetClass;
import org.apache.pdfbox.text.PDFTextStripper;
import org.apache.pdfbox.text.TextPosition;

import java.io.IOException;
import java.io.Writer;
import java.util.ArrayList;
import java.util.List;

/**
 * PDFBox starts writing a page's text with {@code writePageStart}, which Tika's writer of tagged
 * text, whose constructor is private, does not override: it is where {@link TaggedLineOrder}
 * orders that writer's lines before they are written.
 */
@TargetClass(PDFTextStripper.class)
final class Target_org_apache_pdfbox_text_PDFTextStripper {

    @Alias
    protected ArrayList<List<TextPosition>> charactersByArticle;

    @Alias
    protected Writer output;

    @Alias
    public native String getPageStart();

    @Substitute
    protected void writePageStart() throws IOException {
        TaggedLineOrder.arrange((PDFTextStripper) (Object) this, charactersByArticle);
        output.write(getPageStart());
    }
}
