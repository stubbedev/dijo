package org.apache.tika.parser.pdf;

import ai.yobix.ReadingOrder;
import org.apache.pdfbox.pdmodel.PDDocument;
import org.apache.pdfbox.text.TextPosition;
import org.apache.tika.exception.TikaException;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.enricher.CompositeContentEnricher;
import org.apache.tika.parser.pages.PagesConfig;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import java.io.IOException;
import java.io.Writer;
import java.util.List;

/**
 * Tika's writer of untagged PDF text, with each page's glyphs put into {@link ReadingOrder}
 * before PDFBox assembles them into lines and paragraphs. It lives in Tika's package because
 * Tika's writer and the types it takes are package-private.
 */
final class ReadingOrderPDF2XHTML extends PDF2XHTML {

    private ReadingOrderPDF2XHTML(PDDocument document, ContentHandler handler,
                                  ParseContext context, Metadata metadata,
                                  PDFParserConfig config, PagesConfig pages, PageEmitter emitter,
                                  CompositeContentEnricher contentEnrichers) throws IOException {
        super(document, handler, context, metadata, config, pages, emitter, contentEnrichers);
    }

    /** Tika's {@code PDF2XHTML.process}, writing with this writer. */
    static void write(PDDocument document, ContentHandler handler, ParseContext context,
                        Metadata metadata, PDFParserConfig config, PagesConfig pages,
                        PageEmitter emitter, CompositeContentEnricher contentEnrichers)
            throws SAXException, TikaException {
        if (config.isDetectAngles()) {
            throw new TikaException("Detecting the angles of PDF text is not supported");
        }
        final ReadingOrderPDF2XHTML writer;
        try {
            writer = new ReadingOrderPDF2XHTML(document, handler, context, metadata, config,
                    pages, emitter, contentEnrichers);
            config.configure(writer);
            writer.writeText(document, Writer.nullWriter());
        } catch (IOException e) {
            if (e.getCause() instanceof SAXException cause) {
                throw cause;
            }
            throw new TikaException("Unable to extract PDF content", e);
        }
        if (!writer.exceptions.isEmpty()) {
            throw new TikaException("Unable to extract PDF content", writer.exceptions.get(0));
        }
    }

    @Override
    protected void writePage() throws IOException {
        for (final List<TextPosition> article : charactersByArticle) {
            ReadingOrder.arrange(article);
        }
        super.writePage();
    }
}
