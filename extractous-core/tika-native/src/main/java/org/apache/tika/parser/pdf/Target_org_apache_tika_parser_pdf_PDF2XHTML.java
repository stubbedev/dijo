package org.apache.tika.parser.pdf;

import com.oracle.svm.core.annotate.Substitute;
import com.oracle.svm.core.annotate.TargetClass;
import org.apache.pdfbox.pdmodel.PDDocument;
import org.apache.tika.exception.TikaException;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.enricher.CompositeContentEnricher;
import org.apache.tika.parser.pages.PagesConfig;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

/** Tika writes untagged PDF text with {@link ReadingOrderPDF2XHTML}. */
@TargetClass(PDF2XHTML.class)
final class Target_org_apache_tika_parser_pdf_PDF2XHTML {

    @Substitute
    public static void process(PDDocument document, ContentHandler handler, ParseContext context,
                               Metadata metadata, PDFParserConfig config, PagesConfig pages,
                               PageEmitter emitter, CompositeContentEnricher contentEnrichers)
            throws SAXException, TikaException {
        ReadingOrderPDF2XHTML.write(document, handler, context, metadata, config, pages,
                emitter, contentEnrichers);
    }
}
