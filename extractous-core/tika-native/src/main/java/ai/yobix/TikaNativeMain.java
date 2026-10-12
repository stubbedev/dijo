package ai.yobix;

import org.apache.commons.io.IOUtils;
import org.apache.tika.detect.DefaultDetector;
import org.apache.tika.detect.Detector;
import org.apache.tika.exception.EncryptedDocumentException;
import org.apache.tika.exception.TikaException;
import org.apache.tika.exception.WriteLimitReachedException;
import org.apache.tika.extractor.EmbeddedDocumentExtractor;
import org.apache.tika.io.TemporaryResources;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.HttpHeaders;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.metadata.TikaCoreProperties;
import org.apache.tika.mime.MediaType;
import org.apache.tika.mime.MediaTypeRegistry;
import org.apache.tika.parser.AutoDetectParser;
import org.apache.tika.parser.CompositeParser;
import org.apache.tika.parser.DefaultParser;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.parser.ParserDecorator;
import org.apache.tika.parser.PasswordProvider;
import org.apache.tika.parser.RecursiveParserWrapper;
import org.apache.tika.parser.enricher.CompositeContentEnricher;
import org.apache.tika.parser.enricher.EnrichingParser;
import org.apache.tika.parser.microsoft.OfficeParserConfig;
import org.apache.tika.parser.ocr.TesseractOCRConfig;
import org.apache.tika.parser.ocr.TesseractOCRParser;
import org.apache.tika.parser.pdf.PDFParserConfig;
import org.apache.tika.sax.BasicContentHandlerFactory;
import org.apache.tika.sax.BodyContentHandler;
import org.apache.tika.sax.RecursiveParserWrapperHandler;
import org.apache.tika.sax.ToXMLContentHandler;
import org.apache.tika.sax.WriteOutContentHandler;
import org.graalvm.nativeimage.IsolateThread;
import org.graalvm.nativeimage.c.function.CEntryPoint;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.net.URI;
import java.net.URISyntaxException;
import java.nio.ByteBuffer;
import java.nio.charset.Charset;
import java.nio.file.Paths;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HashMap;
import java.util.IdentityHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;

/**
 * The entry points the Rust crate calls through JNI. Every operation exists for a file path, a
 * byte buffer and a URL; failures are returned as a status and a message rather than thrown.
 */
public class TikaNativeMain {

    private static final byte COULD_NOT_OPEN = 1;
    private static final byte PARSE_FAILED = 2;
    private static final byte ENCRYPTED = 3;

    /** Set on the metadata of a document Tika asked a password for. */
    public static final String PASSWORD_USED = "extractous:password-used";

    private static final Detector DETECTOR = new DefaultDetector();
    private static final Parser PARSER = createParser();

    static {
        java.awt.GraphicsEnvironment.isHeadless();
        javax.imageio.ImageIO.getReaderFormatNames();
    }

    /**
     * Tika's parser, mapping each type straight to the parser for it (Tika's default composite
     * rebuilds its map of every parser for each document it is asked to parse), with the formats
     * it handles badly replaced: encrypted CMS and OpenPGP data are reported as encrypted,
     * detached signatures as content-free; subtitles, notebooks, FictionBook books, Apple Mail
     * messages, iWork 2013+ documents and Kindle books are read for their text; and when
     * tesseract is installed, images are turned upright before OCR and PDF pages are OCRed when
     * their text layer is missing or garbage.
     */
    private static Parser createParser() {
        final DefaultParser defaults = new DefaultParser();
        final AutoDetectParser parser = new AutoDetectParser(DETECTOR, defaults);
        final Map<MediaType, Parser> parsers = new HashMap<>(defaults.getParsers());
        final List<Parser> replacements = new ArrayList<>(List.of(new CmsParser(), new OpenPgpParser(),
                new SubtitleParser(), new NotebookParser(), new FictionBookParser(),
                new IworkArchiveParser(), new MobiParser(parsers.get(MediaType.text("html"))),
                new EmlxParser(parsers.get(MediaType.parse("message/rfc822")))));
        final TesseractOCRParser tesseract = tesseract(defaults);
        final List<Parser> recognizers = new ArrayList<>();
        if (tesseract != null) {
            final Parser preparingOcr = new PreparingOcrParser(tesseract);
            recognizers.add(preparingOcr);
            final Parser pdf = new CompositeParser(MediaTypeRegistry.getDefaultRegistry(),
                    parsers.get(MediaType.application("pdf")));
            replacements.add(new TextLayerCheckingPdfParser(pdf, preparingOcr));
        }
        injectTextRecognizers(defaults, new CompositeContentEnricher(recognizers));
        for (final Parser replacement : replacements) {
            for (final MediaType type : replacement.getSupportedTypes(new ParseContext())) {
                parsers.put(type, replacement);
            }
        }
        parser.setParsers(parsers);
        return parser;
    }

    /** Tika's tesseract parser, when tesseract is installed. */
    private static TesseractOCRParser tesseract(DefaultParser defaults) {
        for (final Parser candidate : defaults.getAllComponentParsers()) {
            if (candidate instanceof TesseractOCRParser ocr
                    && !ocr.getSupportedTypes(new ParseContext()).isEmpty()) {
                return ocr;
            }
        }
        return null;
    }

    /** Hands every parser that OCRs its images or rendered pages the given text recognizers. */
    private static void injectTextRecognizers(Parser parser, CompositeContentEnricher recognizers) {
        if (parser instanceof EnrichingParser enriching) {
            enriching.setContentEnrichers(recognizers);
        }
        if (parser instanceof CompositeParser composite) {
            for (final Parser child : composite.getAllComponentParsers()) {
                injectTextRecognizers(child, recognizers);
            }
        } else if (parser instanceof ParserDecorator decorator) {
            injectTextRecognizers(decorator.getWrappedParser(), recognizers);
        }
    }

    @FunctionalInterface
    private interface Source {
        TikaInputStream open(Metadata metadata) throws IOException, URISyntaxException;
    }

    private static Source file(String path) {
        return metadata -> TikaInputStream.get(Paths.get(path), metadata);
    }

    private static Source bytes(ByteBuffer data) {
        return metadata -> TikaInputStream.get(
                new ByteBufferInputStream(data), new TemporaryResources(), metadata);
    }

    private static Source copiedBytes(ByteBuffer data) {
        final byte[] copy = new byte[data.remaining()];
        data.get(copy);
        return metadata -> TikaInputStream.get(copy, metadata);
    }

    private static Source url(String url) {
        return metadata -> TikaInputStream.get(new URI(url).toURL(), metadata);
    }

    private static byte parseFailureStatus(Throwable throwable) {
        final Set<Throwable> seen = Collections.newSetFromMap(new IdentityHashMap<>());
        for (Throwable t = throwable; t != null && seen.add(t); t = t.getCause()) {
            if (t instanceof EncryptedDocumentException) {
                return ENCRYPTED;
            }
        }
        return PARSE_FAILED;
    }

    private static String couldNotOpen(Throwable throwable) {
        return "Could not open the input: " + Throwables.describe(throwable);
    }

    private static String parseFailed(Throwable throwable) {
        return "Parse error occurred : " + Throwables.describe(throwable);
    }

    private static Parser parser(String password) {
        return password == null ? PARSER : new LegacyOfficePasswordParser(PARSER, password);
    }

    private static ParseContext parseContext(
            PDFParserConfig pdfConfig,
            OfficeParserConfig officeConfig,
            TesseractOCRConfig tesseractConfig,
            String password
    ) {
        final ParseContext parseContext = new ParseContext();
        parseContext.set(Parser.class, PARSER);
        parseContext.set(EmbeddedDocumentExtractor.class, new ContentEmbeddedDocumentExtractor());
        parseContext.set(PDFParserConfig.class, pdfConfig);
        parseContext.set(OfficeParserConfig.class, officeConfig);
        parseContext.set(TesseractOCRConfig.class, tesseractConfig);
        if (password != null) {
            parseContext.set(PasswordProvider.class, metadata -> {
                metadata.set(PASSWORD_USED, "true");
                return password;
            });
        }
        return parseContext;
    }

    /**
     * Waits for every parse to end, cancelled ones included, for at most timeoutMillis. Returns
     * whether they all did.
     */
    public static boolean awaitIdleParses(long timeoutMillis) throws InterruptedException {
        return ParseThreads.awaitIdle(timeoutMillis);
    }

    public static StringResult detectFile(String filePath) {
        return detect(file(filePath));
    }

    public static StringResult detectBytes(ByteBuffer data) {
        return detect(bytes(data));
    }

    public static StringResult detectUrl(String url) {
        return detect(url(url));
    }

    /**
     * Detects the media type with Tika's default detector: MIME magic, container inspection and
     * the resource name.
     */
    private static StringResult detect(Source source) {
        final Metadata metadata = new Metadata();
        try (TikaInputStream stream = source.open(metadata)) {
            return new StringResult(DETECTOR.detect(stream, metadata, new ParseContext()).toString(), metadata);
        } catch (IOException | URISyntaxException e) {
            return new StringResult(COULD_NOT_OPEN, couldNotOpen(e));
        }
    }

    public static StringResult parseFileToString(
            String filePath, PDFParserConfig pdfConfig, OfficeParserConfig officeConfig,
            TesseractOCRConfig tesseractConfig, String password, int maxLength, boolean asXML) {
        return parseToString(file(filePath), pdfConfig, officeConfig, tesseractConfig, password,
                maxLength, asXML);
    }

    public static StringResult parseBytesToString(
            ByteBuffer data, PDFParserConfig pdfConfig, OfficeParserConfig officeConfig,
            TesseractOCRConfig tesseractConfig, String password, int maxLength, boolean asXML) {
        return parseToString(bytes(data), pdfConfig, officeConfig, tesseractConfig, password,
                maxLength, asXML);
    }

    public static StringResult parseUrlToString(
            String url, PDFParserConfig pdfConfig, OfficeParserConfig officeConfig,
            TesseractOCRConfig tesseractConfig, String password, int maxLength, boolean asXML) {
        return parseToString(url(url), pdfConfig, officeConfig, tesseractConfig, password,
                maxLength, asXML);
    }

    /**
     * Parses a document into a string of at most maxLength characters.
     */
    private static StringResult parseToString(
            Source source, PDFParserConfig pdfConfig, OfficeParserConfig officeConfig,
            TesseractOCRConfig tesseractConfig, String password, int maxLength, boolean asXML) {
        final Metadata metadata = new Metadata();
        final TikaInputStream stream;
        try {
            stream = source.open(metadata);
        } catch (IOException | URISyntaxException e) {
            return new StringResult(COULD_NOT_OPEN, couldNotOpen(e));
        }

        final ContentHandler handler;
        final ContentHandler handlerForParser;
        if (asXML) {
            handler = new WriteOutContentHandler(new ToXMLContentHandler(), maxLength);
            handlerForParser = handler;
        } else {
            handler = new WriteOutContentHandler(maxLength);
            handlerForParser = new BodyContentHandler(new BlockBreakHandler(handler));
        }
        try (stream) {
            parser(password).parse(stream, handlerForParser, metadata,
                    parseContext(pdfConfig, officeConfig, tesseractConfig, password));
        } catch (SAXException e) {
            if (!WriteLimitReachedException.isWriteLimitReached(e)) {
                return new StringResult(PARSE_FAILED, parseFailed(e));
            }
        } catch (IOException | TikaException e) {
            return new StringResult(parseFailureStatus(e), parseFailed(e));
        }
        return new StringResult(handler.toString(), metadata);
    }

    public static ReaderResult parseFile(
            String filePath, PDFParserConfig pdfConfig, OfficeParserConfig officeConfig,
            TesseractOCRConfig tesseractConfig, String password, String charsetName, boolean asXML,
            long timeoutMillis, String embeddedSpoolDir, long embeddedByteBudget) {
        return parse(file(filePath), pdfConfig, officeConfig, tesseractConfig, password,
                charsetName, asXML, timeoutMillis, embeddedSpoolDir, embeddedByteBudget);
    }

    public static ReaderResult parseBytes(
            ByteBuffer data, PDFParserConfig pdfConfig, OfficeParserConfig officeConfig,
            TesseractOCRConfig tesseractConfig, String password, String charsetName, boolean asXML,
            long timeoutMillis, String embeddedSpoolDir, long embeddedByteBudget) {
        return parse(copiedBytes(data), pdfConfig, officeConfig, tesseractConfig, password,
                charsetName, asXML, timeoutMillis, embeddedSpoolDir, embeddedByteBudget);
    }

    public static ReaderResult parseUrl(
            String url, PDFParserConfig pdfConfig, OfficeParserConfig officeConfig,
            TesseractOCRConfig tesseractConfig, String password, String charsetName, boolean asXML,
            long timeoutMillis, String embeddedSpoolDir, long embeddedByteBudget) {
        return parse(url(url), pdfConfig, officeConfig, tesseractConfig, password,
                charsetName, asXML, timeoutMillis, embeddedSpoolDir, embeddedByteBudget);
    }

    /**
     * Parses a document on a worker thread into a stream of its text encoded with charsetName.
     * Byte input is copied first because the parse outlives this call. Reads wait at most
     * timeoutMillis from now for text (0 for no limit). With an embeddedSpoolDir, documents
     * embedded in this one are copied there, up to embeddedByteBudget bytes in all, instead of
     * being parsed inline.
     */
    private static ReaderResult parse(
            Source source, PDFParserConfig pdfConfig, OfficeParserConfig officeConfig,
            TesseractOCRConfig tesseractConfig, String password, String charsetName, boolean asXML,
            long timeoutMillis, String embeddedSpoolDir, long embeddedByteBudget) {
        final Metadata metadata = new Metadata();
        final TikaInputStream stream;
        try {
            stream = source.open(metadata);
        } catch (IOException | URISyntaxException e) {
            return new ReaderResult(COULD_NOT_OPEN, couldNotOpen(e));
        }
        final ParseContext parseContext =
                parseContext(pdfConfig, officeConfig, tesseractConfig, password);
        final EmbeddedSpooler spooler = embeddedSpoolDir == null
                ? null
                : new EmbeddedSpooler(Paths.get(embeddedSpoolDir), embeddedByteBudget);
        if (spooler != null) {
            parseContext.set(EmbeddedDocumentExtractor.class, spooler);
        }
        try {
            final InputStream text = new ParsingInputStream(
                    parser(password), stream, metadata, parseContext, asXML, Charset.forName(charsetName),
                    timeoutMillis, spooler);
            return new ReaderResult(text, metadata);
        } catch (IOException e) {
            return new ReaderResult(parseFailureStatus(e), parseFailed(e));
        }
    }

    public static EmbeddedExtractResult extractEmbeddedFile(
            String filePath, PDFParserConfig pdfConfig, OfficeParserConfig officeConfig,
            TesseractOCRConfig tesseractConfig, String password) {
        return extractEmbedded(file(filePath), pdfConfig, officeConfig, tesseractConfig, password);
    }

    public static EmbeddedExtractResult extractEmbeddedBytes(
            ByteBuffer data, PDFParserConfig pdfConfig, OfficeParserConfig officeConfig,
            TesseractOCRConfig tesseractConfig, String password) {
        return extractEmbedded(bytes(data), pdfConfig, officeConfig, tesseractConfig, password);
    }

    public static EmbeddedExtractResult extractEmbeddedUrl(
            String url, PDFParserConfig pdfConfig, OfficeParserConfig officeConfig,
            TesseractOCRConfig tesseractConfig, String password) {
        return extractEmbedded(url(url), pdfConfig, officeConfig, tesseractConfig, password);
    }

    /**
     * Collects every document embedded in the input, recursively, like Tika Server's /unpack/all.
     */
    private static EmbeddedExtractResult extractEmbedded(
            Source source, PDFParserConfig pdfConfig, OfficeParserConfig officeConfig,
            TesseractOCRConfig tesseractConfig, String password) {
        final Metadata metadata = new Metadata();
        final TikaInputStream stream;
        try {
            stream = source.open(metadata);
        } catch (IOException | URISyntaxException e) {
            return new EmbeddedExtractResult(COULD_NOT_OPEN, couldNotOpen(e));
        }

        final List<EmbeddedExtractResult.EmbeddedDocument> documents = new ArrayList<>();
        final ParseContext parseContext =
                parseContext(pdfConfig, officeConfig, tesseractConfig, password);
        parseContext.set(EmbeddedDocumentExtractor.class, new EmbeddedDocumentExtractor() {
            @Override
            public boolean shouldParseEmbedded(Metadata embeddedMetadata, ParseContext context) {
                return true;
            }

            @Override
            public void parseEmbedded(TikaInputStream embedded, ContentHandler handler,
                                      Metadata embeddedMetadata, ParseContext context,
                                      boolean outputHtml)
                    throws IOException {
                final String name = embeddedMetadata.get(TikaCoreProperties.RESOURCE_NAME_KEY);
                final String contentType = embeddedMetadata.get(HttpHeaders.CONTENT_TYPE);
                final ByteArrayOutputStream content = new ByteArrayOutputStream();
                IOUtils.copy(embedded, content);
                documents.add(new EmbeddedExtractResult.EmbeddedDocument(
                        name == null ? "embedded_" + documents.size() : name,
                        contentType == null ? "application/octet-stream" : contentType,
                        content.toByteArray(),
                        embeddedMetadata.get(TikaCoreProperties.EMBEDDED_RELATIONSHIP_ID)));
            }
        });

        try (stream) {
            new RecursiveParserWrapper(parser(password)).parse(stream,
                    new RecursiveParserWrapperHandler(new BasicContentHandlerFactory(
                            BasicContentHandlerFactory.HANDLER_TYPE.TEXT, -1)),
                    metadata, parseContext);
            return new EmbeddedExtractResult(documents, metadata);
        } catch (IOException | SAXException | TikaException e) {
            return new EmbeddedExtractResult(parseFailureStatus(e), parseFailed(e));
        }
    }

    /**
     * The C entry point native-image needs to build a shared library rather than an executable.
     * extractous calls the library through JNI instead.
     */
    @CEntryPoint(name = "extractous_isolate_ready")
    private static boolean isolateReady(IsolateThread thread) {
        return true;
    }
}
