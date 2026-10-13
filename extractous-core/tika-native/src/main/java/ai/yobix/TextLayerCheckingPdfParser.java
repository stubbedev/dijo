package ai.yobix;

import org.apache.pdfbox.Loader;
import org.apache.pdfbox.io.IOUtils;
import org.apache.pdfbox.pdmodel.PDDocument;
import org.apache.pdfbox.pdmodel.common.PDRectangle;
import org.apache.pdfbox.rendering.ImageType;
import org.apache.pdfbox.rendering.PDFRenderer;
import org.apache.tika.exception.TikaException;
import org.apache.tika.io.TemporaryResources;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.metadata.PDF;
import org.apache.tika.metadata.Property;
import org.apache.tika.metadata.TikaCoreProperties;
import org.apache.tika.metadata.TikaPagedText;
import org.apache.tika.mime.MediaType;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.parser.PasswordProvider;
import org.apache.tika.parser.pdf.PDFParserConfig;
import org.apache.tika.parser.pages.PagesConfig;
import org.apache.tika.parser.pages.TextPolicy;
import org.apache.tika.renderer.RenderSettings;
import org.apache.tika.sax.BodyContentHandler;
import org.apache.tika.sax.ContentHandlerDecorator;
import org.apache.tika.sax.EmbeddedContentHandler;
import org.xml.sax.Attributes;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;
import org.xml.sax.helpers.AttributesImpl;
import org.xml.sax.helpers.DefaultHandler;

import javax.imageio.ImageIO;
import java.awt.image.BufferedImage;
import java.io.Closeable;
import java.io.IOException;
import java.io.InputStream;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.Set;

/**
 * Tika's PDF parser, with OCR under the automatic strategy decided page by page from the text the
 * page's text layer yields ({@link TextLayerQuality}), instead of from glyph counts alone. A page
 * with next to no text gets OCR text added, as Tika does. A page whose text layer is garbage has
 * it replaced by OCR text, and a doubtful one is OCRed too and keeps whichever text reads better;
 * either way a replaced text layer is listed in {@link #REJECTED_TEXT_LAYER}.
 */
final class TextLayerCheckingPdfParser implements Parser {

    /** Each page whose text layer was replaced by OCR text, as "page:fault". */
    static final String REJECTED_TEXT_LAYER = "extractous:rejected-text-layer";

    private static final long MAX_PIXELS = 60_000_000L;
    private static final double POINTS_PER_INCH = 72;
    private static final String OCR_IMAGE_TYPE = MediaType.image("png").toString();

    private final Parser pdf;
    private final Parser ocr;

    /**
     * @param pdf parses PDFs, recording which parser it is
     * @param ocr reads the text in an image
     */
    TextLayerCheckingPdfParser(Parser pdf, Parser ocr) {
        this.pdf = pdf;
        this.ocr = ocr;
    }

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return pdf.getSupportedTypes(context);
    }

    /** How pages are OCRed, fixed by the outermost PDF and kept for the PDFs inside it. */
    private record PageOcr(PagesConfig.Auto thresholds, int dpi, ImageType imageType) {

        static PageOcr of(PagesConfig pages) {
            final RenderSettings render = pages.getRender();
            final ImageType imageType = switch (render.getImageType()) {
                case GRAY -> ImageType.GRAY;
                case RGB -> ImageType.RGB;
            };
            return new PageOcr(pages.getOcr().getAuto(), render.getDpi(), imageType);
        }
    }

    @Override
    public void parse(TikaInputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        final PDFParserConfig config = context.get(PDFParserConfig.class);
        final PageOcr enclosing = context.get(PageOcr.class);
        final PagesConfig pages =
                config == null ? null : PagesConfig.resolve(context, config.getPages());
        final boolean auto = pages != null && pages.getText() == TextPolicy.AUTO;
        if (enclosing == null && !auto) {
            pdf.parse(stream, handler, metadata, context);
            return;
        }
        final PageOcr pageOcr = enclosing != null ? enclosing : PageOcr.of(pages);
        try (TemporaryResources temporary = new TemporaryResources()) {
            final TikaInputStream input = TikaInputStream.get(stream, temporary, metadata);
            try (PageRenderer renderer =
                         new PageRenderer(input.getPath(), pageOcr, metadata, context)) {
                final PageChecker checker =
                        new PageChecker(handler, metadata, context, pageOcr, renderer, temporary);
                if (enclosing == null) {
                    config.pages().setText(TextPolicy.EXTRACT);
                    context.set(PageOcr.class, pageOcr);
                }
                try {
                    pdf.parse(input, checker, metadata, context);
                } finally {
                    if (enclosing == null) {
                        config.pages().setText(TextPolicy.AUTO);
                        context.set(PageOcr.class, null);
                    }
                }
            }
        }
    }

    /** Renders pages of the PDF for OCR, opening it a second time only once a page needs it. */
    private static final class PageRenderer implements Closeable {
        private final Path path;
        private final PageOcr pageOcr;
        private final Metadata metadata;
        private final ParseContext context;
        private PDDocument document;
        private PDFRenderer renderer;

        PageRenderer(Path path, PageOcr pageOcr, Metadata metadata, ParseContext context) {
            this.path = path;
            this.pageOcr = pageOcr;
            this.metadata = metadata;
            this.context = context;
        }

        BufferedImage render(int pageIndex) throws IOException {
            if (document == null) {
                final PasswordProvider passwords = context.get(PasswordProvider.class);
                final String password = passwords == null ? null : passwords.getPassword(metadata);
                document = Loader.loadPDF(path.toFile(), password == null ? "" : password,
                        IOUtils.createTempFileOnlyStreamCache());
                renderer = new PDFRenderer(document);
            }
            final PDRectangle box = document.getPage(pageIndex).getCropBox();
            final double squareInches = box.getWidth() / POINTS_PER_INCH
                    * (box.getHeight() / POINTS_PER_INCH);
            final double dpi = Math.min(pageOcr.dpi(), Math.sqrt(MAX_PIXELS / squareInches));
            return renderer.renderImageWithDPI(pageIndex, (float) dpi, pageOcr.imageType());
        }

        @Override
        public void close() throws IOException {
            if (document != null) {
                document.close();
            }
        }
    }

    /**
     * Holds each page back until it ends, then passes it on as it is, with OCR text added, or
     * with its text replaced by OCR text.
     */
    private final class PageChecker extends ContentHandlerDecorator {
        private final ContentHandler downstream;
        private final Metadata metadata;
        private final ParseContext context;
        private final PageOcr pageOcr;
        private final PageRenderer renderer;
        private final TemporaryResources temporary;
        private int pageIndex;
        private Recording page;
        private int depth;

        PageChecker(ContentHandler handler, Metadata metadata, ParseContext context,
                    PageOcr pageOcr, PageRenderer renderer, TemporaryResources temporary) {
            super(handler);
            this.downstream = handler;
            this.metadata = metadata;
            this.context = context;
            this.pageOcr = pageOcr;
            this.renderer = renderer;
            this.temporary = temporary;
        }

        @Override
        public void startElement(String uri, String localName, String name, Attributes atts)
                throws SAXException {
            if (page != null) {
                depth++;
                page.startElement(uri, localName, name, atts);
                return;
            }
            super.startElement(uri, localName, name, atts);
            if ("div".equals(localName) && "page".equals(atts.getValue("class"))) {
                page = new Recording();
                depth = 0;
            }
        }

        @Override
        public void endElement(String uri, String localName, String name) throws SAXException {
            if (page == null) {
                super.endElement(uri, localName, name);
                return;
            }
            if (depth > 0) {
                depth--;
                page.endElement(uri, localName, name);
                return;
            }
            final Recording ended = page;
            page = null;
            endPage(ended);
            super.endElement(uri, localName, name);
        }

        @Override
        public void characters(char[] ch, int start, int length) throws SAXException {
            if (page != null) {
                page.characters(ch, start, length);
            } else {
                super.characters(ch, start, length);
            }
        }

        @Override
        public void ignorableWhitespace(char[] ch, int start, int length) throws SAXException {
            if (page != null) {
                page.ignorableWhitespace(ch, start, length);
            } else {
                super.ignorableWhitespace(ch, start, length);
            }
        }

        @Override
        public void startPrefixMapping(String prefix, String uri) throws SAXException {
            if (page != null) {
                page.startPrefixMapping(prefix, uri);
            } else {
                super.startPrefixMapping(prefix, uri);
            }
        }

        @Override
        public void endPrefixMapping(String prefix) throws SAXException {
            if (page != null) {
                page.endPrefixMapping(prefix);
            } else {
                super.endPrefixMapping(prefix);
            }
        }

        @Override
        public void processingInstruction(String target, String data) throws SAXException {
            if (page != null) {
                page.processingInstruction(target, data);
            } else {
                super.processingInstruction(target, data);
            }
        }

        @Override
        public void skippedEntity(String name) throws SAXException {
            if (page != null) {
                page.skippedEntity(name);
            } else {
                super.skippedEntity(name);
            }
        }

        private void endPage(Recording text) throws SAXException {
            final int index = pageIndex++;
            final int glyphs = lastInt(PDF.CHARACTERS_PER_PAGE, index);
            final int unmapped = lastInt(PDF.UNMAPPED_UNICODE_CHARS_PER_PAGE, index);
            final PagesConfig.Auto thresholds = pageOcr.thresholds();
            if (glyphs <= thresholds.getTotalCharsPerPage()) {
                text.replay(downstream);
                final Recording ocrText = ocr(index);
                if (ocrText != null) {
                    useOcr(ocrText);
                }
                return;
            }
            final float unmappedLimit = thresholds.getUnmappedUnicodeCharsPerPage();
            final TextLayerQuality.Assessment layer =
                    TextLayerQuality.assess(text.text(), glyphs, unmapped, unmappedLimit);
            final Recording ocrText =
                    layer.badness() < TextLayerQuality.DOUBTFUL ? null : ocr(index);
            if (ocrText == null || !(layer.badness() >= TextLayerQuality.GARBAGE
                    || readsBetter(ocrText, layer, unmappedLimit))) {
                text.replay(downstream);
                return;
            }
            metadata.add(REJECTED_TEXT_LAYER, (index + 1) + ":" + layer.fault());
            useOcr(ocrText);
        }

        /** Passes on a page's OCR text, with the report of how its image was read. */
        private void useOcr(Recording ocrText) throws SAXException {
            ocrText.replay(downstream);
            for (final String report : ocrText.reports) {
                metadata.add(OcrReport.KEY, report);
            }
        }

        private boolean readsBetter(Recording ocrText, TextLayerQuality.Assessment layer,
                                    float unmappedLimit) {
            final String text = ocrText.text();
            return TextLayerQuality.assess(text, text.length(), 0, unmappedLimit).badness()
                    < layer.badness();
        }

        private int lastInt(Property property, int index) {
            final String[] values = metadata.getValues(property);
            if (values.length == 0) {
                throw new IllegalStateException("Tika reported no " + property.getName()
                        + " for page " + (index + 1));
            }
            return Integer.parseInt(values[values.length - 1]);
        }

        /** The OCR text of a page, or null when it could not be read, with the reason noted. */
        private Recording ocr(int index) throws SAXException {
            final Recording ocrText = new Recording();
            try {
                final Path image = temporary.createTempFile(".png");
                if (!ImageIO.write(renderer.render(index), "png", image.toFile())) {
                    throw new IOException("No PNG writer to render a page for OCR");
                }
                final Metadata pageMetadata = new Metadata();
                pageMetadata.set(TikaCoreProperties.CONTENT_TYPE_PARSER_OVERRIDE, OCR_IMAGE_TYPE);
                pageMetadata.set(TikaPagedText.PAGE_NUMBER, index + 1);
                try (TikaInputStream input = TikaInputStream.get(image)) {
                    ocr.parse(input, new EmbeddedContentHandler(new BodyContentHandler(ocrText)),
                            pageMetadata, context);
                }
                ocrText.reports = pageMetadata.getValues(OcrReport.KEY);
                return ocrText;
            } catch (IOException | TikaException e) {
                metadata.add(TikaCoreProperties.TIKA_META_EXCEPTION_WARNING,
                        "OCR of page " + (index + 1) + " failed: " + Throwables.describe(e));
                return null;
            }
        }
    }

    /** SAX events kept to be passed on later, and the text among them. */
    private static final class Recording extends DefaultHandler {

        @FunctionalInterface
        private interface Event {
            void replay(ContentHandler handler) throws SAXException;
        }

        private final List<Event> events = new ArrayList<>();
        private final StringBuilder text = new StringBuilder();
        private String[] reports = new String[0];

        String text() {
            return text.toString();
        }

        void replay(ContentHandler handler) throws SAXException {
            for (final Event event : events) {
                event.replay(handler);
            }
        }

        @Override
        public void startElement(String uri, String localName, String name, Attributes atts) {
            final Attributes copy = new AttributesImpl(atts);
            events.add(handler -> handler.startElement(uri, localName, name, copy));
        }

        @Override
        public void endElement(String uri, String localName, String name) {
            events.add(handler -> handler.endElement(uri, localName, name));
        }

        @Override
        public void characters(char[] ch, int start, int length) {
            final char[] copy = Arrays.copyOfRange(ch, start, start + length);
            text.append(copy);
            events.add(handler -> handler.characters(copy, 0, copy.length));
        }

        @Override
        public void ignorableWhitespace(char[] ch, int start, int length) {
            final char[] copy = Arrays.copyOfRange(ch, start, start + length);
            text.append(copy);
            events.add(handler -> handler.ignorableWhitespace(copy, 0, copy.length));
        }

        @Override
        public void startPrefixMapping(String prefix, String uri) {
            events.add(handler -> handler.startPrefixMapping(prefix, uri));
        }

        @Override
        public void endPrefixMapping(String prefix) {
            events.add(handler -> handler.endPrefixMapping(prefix));
        }

        @Override
        public void processingInstruction(String target, String data) {
            events.add(handler -> handler.processingInstruction(target, data));
        }

        @Override
        public void skippedEntity(String name) {
            events.add(handler -> handler.skippedEntity(name));
        }
    }
}
