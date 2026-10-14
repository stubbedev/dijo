package ai.yobix;

import org.apache.tika.exception.TikaException;
import org.apache.tika.exception.TikaTimeoutException;
import org.apache.tika.io.TemporaryResources;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.stream.Collectors;

/**
 * Tesseract run as a program for every image, reading the language data in a directory, or its
 * own when none is named.
 */
final class TesseractProgram implements Tesseract {

    private static final long CHECK_TIMEOUT_MILLIS = 60_000;
    private static final String TESSDATA_PREFIX = "TESSDATA_PREFIX";
    private static final String OMP_NUM_THREADS = "OMP_NUM_THREADS";
    private static final String OSD_PAGE_SEG_MODE = "0";
    private static final String TEXT_EXTENSION = ".txt";
    private static final String TSV_EXTENSION = ".tsv";
    private static final String TSV_WORD_LEVEL = "5";
    private static final int TSV_CONFIDENCE = 10;

    private final String program;
    private final String tessdata;
    private Set<String> languages;

    /**
     * @param program  the executable
     * @param tessdata the language data, or null for the program's own
     */
    TesseractProgram(String program, String tessdata) {
        this.program = program;
        this.tessdata = tessdata;
    }

    /** The tesseract on the PATH. */
    static TesseractProgram onPath() {
        return new TesseractProgram(System.getProperty("os.name").startsWith("Windows")
                ? "tesseract.exe" : "tesseract", null);
    }

    /** Whether the program starts and runs, which it does not when it is not installed. */
    boolean runs() {
        try {
            return run(CHECK_TIMEOUT_MILLIS, Map.of(), "--version").succeeded();
        } catch (IOException e) {
            return false;
        }
    }

    /**
     * Writes the text and, for the confidence, the TSV renderer's word table beside each other,
     * as {@code <base>.txt} and {@code <base>.tsv}: asking for the TSV turns off the text the
     * program writes by default, so both are asked for.
     */
    @Override
    public Reading read(OcrImage ocrImage, String pageSegMode, String language,
                        Map<String, String> variables, long timeoutMillis)
            throws IOException, TikaException {
        try (TemporaryResources temporary = new TemporaryResources()) {
            final Path image = ocrImage.file(temporary);
            final Path base = temporary.createTempFile();
            final Path text = base.resolveSibling(base.getFileName() + TEXT_EXTENSION);
            final Path words = base.resolveSibling(base.getFileName() + TSV_EXTENSION);
            final List<String> arguments = new ArrayList<>(
                    List.of(image.toString(), base.toString(), "--psm", pageSegMode));
            if (language != null && !language.isBlank()) {
                arguments.add("-l");
                arguments.add(language);
            }
            addVariables(arguments, variables);
            arguments.addAll(List.of("-c", "tessedit_create_txt=1", "-c", "tessedit_create_tsv=1"));
            try (ReadingThreads threads = ReadingThreads.take()) {
                final Programs.Outcome outcome = run(timeoutMillis,
                        Map.of(OMP_NUM_THREADS, Integer.toString(threads.threads())),
                        arguments.toArray(String[]::new));
                if (!outcome.finished()) {
                    throw new TikaTimeoutException("Reading " + image + " failed: "
                            + outcome.failure());
                }
                if (!outcome.succeeded()) {
                    throw new TikaException("Reading " + image + " failed: " + outcome.failure());
                }
                return new Reading(Files.readString(text, StandardCharsets.UTF_8),
                        meanConfidence(Files.readAllLines(words, StandardCharsets.UTF_8)));
            } finally {
                Files.deleteIfExists(text);
                Files.deleteIfExists(words);
            }
        }
    }

    /**
     * The mean confidence of the words in a TSV word table, as tesseract's MeanTextConf takes it:
     * the integer mean of the word rows' confidences, 0 without words.
     */
    static int meanConfidence(List<String> table) {
        int sum = 0;
        int count = 0;
        for (final String row : table) {
            final String[] cells = row.split("\t", -1);
            if (cells.length > TSV_CONFIDENCE && TSV_WORD_LEVEL.equals(cells[0])) {
                sum += (int) Float.parseFloat(cells[TSV_CONFIDENCE]);
                count++;
            }
        }
        return count == 0 ? 0 : sum / count;
    }

    @Override
    public String detect(OcrImage image, Map<String, String> variables, long timeoutMillis)
            throws IOException {
        try (TemporaryResources temporary = new TemporaryResources()) {
            final List<String> arguments = new ArrayList<>(List.of(
                    image.file(temporary).toString(), "stdout", "--psm", OSD_PAGE_SEG_MODE));
            addVariables(arguments, variables);
            final Programs.Outcome outcome =
                    run(timeoutMillis, Map.of(), arguments.toArray(String[]::new));
            return outcome.succeeded() ? outcome.output() : null;
        }
    }

    private static void addVariables(List<String> arguments, Map<String, String> variables) {
        variables.forEach((name, value) -> {
            arguments.add("-c");
            arguments.add(name + "=" + value);
        });
    }

    @Override
    public synchronized Set<String> languages() throws IOException {
        if (languages == null) {
            final Programs.Outcome listing = run(CHECK_TIMEOUT_MILLIS, Map.of(), "--list-langs");
            if (!listing.succeeded()) {
                throw new IOException("Listing tesseract's languages failed: "
                        + listing.failure());
            }
            languages = listing.output().lines()
                    .skip(1)
                    .map(line -> line.trim().replace('\\', '/'))
                    .filter(line -> !line.isEmpty())
                    .collect(Collectors.toUnmodifiableSet());
        }
        return languages;
    }

    /** Runs the program with its language data and further environment variables. */
    private Programs.Outcome run(long timeoutMillis, Map<String, String> variables,
                                 String... arguments) throws IOException {
        final String[] command = new String[arguments.length + 1];
        command[0] = program;
        System.arraycopy(arguments, 0, command, 1, arguments.length);
        return Programs.run(timeoutMillis, environment -> {
            if (tessdata != null) {
                environment.put(TESSDATA_PREFIX, tessdata);
            }
            environment.putAll(variables);
        }, command);
    }
}
