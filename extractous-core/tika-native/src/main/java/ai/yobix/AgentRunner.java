package ai.yobix;

import org.apache.tika.parser.microsoft.OfficeParserConfig;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.stream.Stream;

/**
 * Detects, parses to text and to XHTML, and unpacks every file under the given paths the way
 * extractous does, so that GraalVM's native-image agent sees every reflective call Tika makes.
 */
public final class AgentRunner {

    private static final String AUTO_OCR_PAGES = "50%,10";
    private static final long TIMEOUT_MILLIS = 120_000;
    private static final int MAX_LENGTH = 1_000_000;

    public static void main(String[] args) throws IOException {
        if (args.length == 0) {
            System.err.println("Pass the files or directories to parse");
            System.exit(2);
        }
        int failures = 0;
        int files = 0;
        for (final String arg : args) {
            final List<Path> paths;
            try (Stream<Path> walk = Files.walk(Path.of(arg))) {
                paths = walk.filter(Files::isRegularFile).sorted().toList();
            }
            for (final Path path : paths) {
                files++;
                failures += exercise(path.toString());
            }
        }
        System.out.println("Exercised " + files + " files, " + failures + " parses failed");
    }

    private static int exercise(String path) throws IOException {
        TikaNativeMain.detectFile(path);
        int failures = 0;
        for (final boolean asXml : new boolean[]{false, true}) {
            final ReaderResult parsed = TikaNativeMain.parseFile(path, pdfConfig(),
                    new OfficeParserConfig(), ocrConfig(), null, StandardCharsets.UTF_8.name(),
                    asXml, TIMEOUT_MILLIS, null, 0);
            if (parsed.isError()) {
                failures += failed(path, "parse", parsed.getErrorMessage());
                continue;
            }
            try (InputStream text = parsed.getStream()) {
                text.transferTo(OutputStream.nullOutputStream());
            } catch (IOException e) {
                failures += failed(path, "read", Throwables.describe(e));
            }
        }
        final StringResult string = TikaNativeMain.parseFileToString(path, pdfConfig(),
                new OfficeParserConfig(), ocrConfig(), null, MAX_LENGTH, false);
        if (string.isError()) {
            failures += failed(path, "parse to string", string.getErrorMessage());
        }
        final EmbeddedExtractResult embedded = TikaNativeMain.extractEmbeddedFile(path,
                pdfConfig(), new OfficeParserConfig(), ocrConfig(), null);
        if (embedded.isError()) {
            failures += failed(path, "unpack", embedded.getErrorMessage());
        }
        return failures;
    }

    private static int failed(String path, String action, String error) {
        System.err.println("Could not " + action + " " + path + ": " + error);
        return 1;
    }

    private static PdfConfig pdfConfig() {
        final PdfConfig config = new PdfConfig();
        config.setOcrStrategy("AUTO");
        config.setOcrStrategyAuto(AUTO_OCR_PAGES);
        return config;
    }

    private static OcrConfig ocrConfig() {
        final OcrConfig config = new OcrConfig();
        config.setLanguage(OcrConfig.AUTO_LANGUAGE);
        return config;
    }
}
