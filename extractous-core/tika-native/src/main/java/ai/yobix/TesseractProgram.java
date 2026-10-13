package ai.yobix;

import org.apache.tika.exception.TikaException;
import org.apache.tika.exception.TikaTimeoutException;

import java.io.IOException;
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
    private static final String OSD_PAGE_SEG_MODE = "0";

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
            return run(CHECK_TIMEOUT_MILLIS, "--version").succeeded();
        } catch (IOException e) {
            return false;
        }
    }

    @Override
    public String read(Path image, String pageSegMode, String language,
                       Map<String, String> variables, long timeoutMillis)
            throws IOException, TikaException {
        final List<String> arguments = new ArrayList<>(
                List.of(image.toString(), "stdout", "--psm", pageSegMode));
        if (language != null && !language.isBlank()) {
            arguments.add("-l");
            arguments.add(language);
        }
        variables.forEach((name, value) -> {
            arguments.add("-c");
            arguments.add(name + "=" + value);
        });
        arguments.add("txt");
        final Programs.Outcome outcome = run(timeoutMillis, arguments.toArray(String[]::new));
        if (!outcome.finished()) {
            throw new TikaTimeoutException("Reading " + image + " failed: " + outcome.failure());
        }
        if (!outcome.succeeded()) {
            throw new TikaException("Reading " + image + " failed: " + outcome.failure());
        }
        return outcome.output();
    }

    @Override
    public String detect(Path image, long timeoutMillis) throws IOException {
        final Programs.Outcome outcome = run(timeoutMillis, image.toString(), "stdout", "--psm",
                OSD_PAGE_SEG_MODE);
        return outcome.succeeded() ? outcome.output() : null;
    }

    @Override
    public synchronized Set<String> languages() throws IOException {
        if (languages == null) {
            final Programs.Outcome listing = run(CHECK_TIMEOUT_MILLIS, "--list-langs");
            if (!listing.succeeded()) {
                throw new IOException("Listing tesseract's languages failed: "
                        + listing.failure());
            }
            languages = listing.output().lines()
                    .skip(1)
                    .map(String::trim)
                    .filter(line -> !line.isEmpty())
                    .collect(Collectors.toUnmodifiableSet());
        }
        return languages;
    }

    private Programs.Outcome run(long timeoutMillis, String... arguments) throws IOException {
        final String[] command = new String[arguments.length + 1];
        command[0] = program;
        System.arraycopy(arguments, 0, command, 1, arguments.length);
        return Programs.run(timeoutMillis, environment -> {
            if (tessdata != null) {
                environment.put(TESSDATA_PREFIX, tessdata);
            }
        }, command);
    }
}
