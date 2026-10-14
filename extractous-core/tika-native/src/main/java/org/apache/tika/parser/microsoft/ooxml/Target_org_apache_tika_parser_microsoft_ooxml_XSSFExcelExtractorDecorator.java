package org.apache.tika.parser.microsoft.ooxml;

import com.oracle.svm.core.annotate.Substitute;
import com.oracle.svm.core.annotate.TargetClass;
import org.apache.poi.hssf.extractor.ExcelExtractor;
import org.apache.poi.hssf.usermodel.HeaderFooter;
import org.apache.tika.sax.XHTMLContentHandler;
import org.xml.sax.SAXException;

/**
 * Tika writes a spreadsheet's print headers and footers with their field codes, such as
 * {@code &12&A} (the sheet's name in 12 point) or {@code &12Page &P}, LibreOffice's defaults,
 * which no reader of the text wants. The codes are stripped, and a header or footer with nothing
 * else is left out.
 */
@TargetClass(XSSFExcelExtractorDecorator.class)
final class Target_org_apache_tika_parser_microsoft_ooxml_XSSFExcelExtractorDecorator {

    @Substitute
    protected void extractHeaderFooter(String headerFooter, XHTMLContentHandler xhtml)
            throws SAXException {
        final String text = HeaderFooter.stripFields(ExcelExtractor._extractHeaderFooter(
                new XSSFExcelExtractorDecorator.HeaderFooterFromString(headerFooter))).strip();
        if (!text.isEmpty()) {
            xhtml.element("p", text);
        }
    }
}
