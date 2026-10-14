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
import org.apache.tika.parser.ocr.TesseractOCRConfig;
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
import java.util.function.Function;

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
                    checker.abandon();
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
     * with its text replaced by OCR text. Pages that need OCR are rendered here, as the parse
     * reaches them, and read on {@link PageReadings} side by side while the parse goes on: the
     * first as any image is, the rest first in the script it leaves the extraction assuming
     * ({@link PageReadings.FirstPage}), so that every page reads the same whichever finishes
     * first. Their output, and everything after it, is passed on in document order as the
     * readings finish, and the extraction assumes the script the first page learned once the
     * document ends.
     */
    private final class PageChecker extends ContentHandlerDecorator {
        private final ContentHandler downstream;
        private final Metadata metadata;
        private final PageOcr pageOcr;
        private final PageRenderer renderer;
        private final TemporaryResources temporary;
        private final TesseractOCRConfig ocrConfig;
        private final PageReadings.InOrder pending = new PageReadings.InOrder();
        private PageReadings.FirstPage first;
        private Recording between;
        private int pageIndex;
        private Recording page;
        private int depth;

        PageChecker(ContentHandler handler, Metadata metadata, ParseContext context,
                    PageOcr pageOcr, PageRenderer renderer, TemporaryResources temporary) {
            super(handler);
            this.downstream = handler;
            this.metadata = metadata;
            this.pageOcr = pageOcr;
            this.renderer = renderer;
            this.temporary = temporary;
            this.ocrConfig = context.get(TesseractOCRConfig.class);
        }

        /** Where an event outside a page goes: on, unless pages before it are still read. */
        private ContentHandler outside() {
            if (pending.isEmpty()) {
                return downstream;
            }
            if (between == null) {
                between = new Recording();
            }
            return between;
        }

        @Override
        public void startElement(String uri, String localName, String name, Attributes atts)
                throws SAXException {
            if (page != null) {
                depth++;
                page.startElement(uri, localName, name, atts);
                return;
            }
            outside().startElement(uri, localName, name, atts);
            if ("div".equals(localName) && "page".equals(atts.getValue("class"))) {
                page = new Recording();
                depth = 0;
            }
        }

        @Override
        public void endElement(String uri, String localName, String name) throws SAXException {
            if (page == null) {
                outside().endElement(uri, localName, name);
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
            outside().endElement(uri, localName, name);
        }

        @Override
        public void characters(char[] ch, int start, int length) throws SAXException {
            (page != null ? page : outside()).characters(ch, start, length);
        }

        @Override
        public void ignorableWhitespace(char[] ch, int start, int length) throws SAXException {
            (page != null ? page : outside()).ignorableWhitespace(ch, start, length);
        }

        @Override
        public void startPrefixMapping(String prefix, String uri) throws SAXException {
            (page != null ? page : outside()).startPrefixMapping(prefix, uri);
        }

        @Override
        public void endPrefixMapping(String prefix) throws SAXException {
            (page != null ? page : outside()).endPrefixMapping(prefix);
        }

        @Override
        public void processingInstruction(String target, String data) throws SAXException {
            (page != null ? page : outside()).processingInstruction(target, data);
        }

        @Override
        public void skippedEntity(String name) throws SAXException {
            (page != null ? page : outside()).skippedEntity(name);
        }

        @Override
        public void endDocument() throws SAXException {
            passAll();
            if (first != null && first.learned() != null && ocrConfig instanceof OcrConfig auto) {
                auto.assumeScript(first.learned());
            }
            super.endDocument();
        }

        private void endPage(Recording text) throws SAXException {
            final int index = pageIndex++;
            final int glyphs = lastInt(PDF.CHARACTERS_PER_PAGE, index);
            final int unmapped = lastInt(PDF.UNMAPPED_UNICODE_CHARS_PER_PAGE, index);
            final PagesConfig.Auto thresholds = pageOcr.thresholds();
            if (glyphs <= thresholds.getTotalCharsPerPage()) {
                read(index, ocrText -> () -> {
                    text.replay(downstream);
                    if (ocrText != null) {
                        useOcr(ocrText);
                    }
                });
                return;
            }
            final float unmappedLimit = thresholds.getUnmappedUnicodeCharsPerPage();
            final TextLayerQuality.Assessment layer =
                    TextLayerQuality.assess(text.text(), glyphs, unmapped, unmappedLimit);
            if (layer.badness() < TextLayerQuality.DOUBTFUL) {
                queue(() -> text.replay(downstream));
                return;
            }
            read(index, ocrText -> () -> {
                if (ocrText == null || !(layer.badness() >= TextLayerQuality.GARBAGE
                        || readsBetter(ocrText, layer, unmappedLimit))) {
                    text.replay(downstream);
                    return;
                }
                metadata.add(REJECTED_TEXT_LAYER, (index + 1) + ":" + layer.fault());
                useOcr(ocrText);
            });
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

        /**
         * Renders a page and queues the output `then` makes of its OCR text, or of null when it
         * could not be rendered or read, with the reason noted when the output is passed on.
         */
        private void read(int index, Function<Recording, PageReadings.Output> then)
                throws SAXException {
            final Path image;
            try {
                image = temporary.createTempFile(".png");
                if (!ImageIO.write(renderer.render(index), "png", image.toFile())) {
                    throw new IOException("No PNG writer to render a page for OCR");
                }
            } catch (IOException e) {
                queue(failed(index, e, then));
                return;
            }
            flushBetween();
            if (first != null) {
                final PageReadings.FirstPage settling = first;
                pending.read(() -> settling.later(script -> output(index, image, script, then)));
                return;
            }
            final PageReadings.FirstPage settling = new PageReadings.FirstPage(new AssumedScript(
                    ocrConfig instanceof OcrConfig auto && auto.isAutoLanguage()
                            ? auto.assumedScript() : null));
            first = settling;
            pending.read(() -> {
                try {
                    final Metadata pageMetadata = new Metadata();
                    final PageReadings.Output output =
                            output(index, image, settling.assumed(), pageMetadata, then);
                    settling.settle(pageMetadata.get(AssumedScript.LEARNED));
                    return output;
                } finally {
                    settling.settle(null);
                }
            });
        }

        private PageReadings.Output output(int index, Path image, AssumedScript assumed,
                                           Function<Recording, PageReadings.Output> then)
                throws SAXException {
            return output(index, image, assumed, new Metadata(), then);
        }

        /** Reads a rendered page, into its own metadata, and makes `then`'s output of it. */
        private PageReadings.Output output(int index, Path image, AssumedScript assumed,
                                           Metadata pageMetadata,
                                           Function<Recording, PageReadings.Output> then)
                throws SAXException {
            final Recording ocrText = new Recording();
            pageMetadata.set(TikaCoreProperties.CONTENT_TYPE_PARSER_OVERRIDE, OCR_IMAGE_TYPE);
            pageMetadata.set(TikaPagedText.PAGE_NUMBER, index + 1);
            final ParseContext pageContext = new ParseContext();
            if (ocrConfig != null) {
                pageContext.set(TesseractOCRConfig.class, ocrConfig);
            }
            pageContext.set(AssumedScript.class, assumed);
            try (TikaInputStream input = TikaInputStream.get(image)) {
                ocr.parse(input, new EmbeddedContentHandler(new BodyContentHandler(ocrText)),
                        pageMetadata, pageContext);
            } catch (IOException | TikaException e) {
                return failed(index, e, then);
            }
            ocrText.reports = pageMetadata.getValues(OcrReport.KEY);
            return then.apply(ocrText);
        }

        private PageReadings.Output failed(int index, Exception e,
                                           Function<Recording, PageReadings.Output> then) {
            final PageReadings.Output output = then.apply(null);
            return () -> {
                metadata.add(TikaCoreProperties.TIKA_META_EXCEPTION_WARNING,
                        "OCR of page " + (index + 1) + " failed: " + Throwables.describe(e));
                output.pass();
            };
        }

        /** Queues a page's output that is ready now behind what is queued already. */
        private void queue(PageReadings.Output output) throws SAXException {
            flushBetween();
            pending.ready(output);
        }

        /** Queues what passed outside pages while pages before it were read. */
        private void flushBetween() throws SAXException {
            if (between != null) {
                final Recording events = between;
                between = null;
                pending.ready(() -> events.replay(downstream));
            }
        }

        /** Passes on every queued page, in order, as its reading finishes. */
        private void passAll() throws SAXException {
            flushBetween();
            pending.passAll();
        }

        void abandon() {
            if (first != null) {
                first.settle(null);
            }
            pending.abandon();
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
