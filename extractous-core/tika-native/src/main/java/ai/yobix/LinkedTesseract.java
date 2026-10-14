package ai.yobix;

import org.apache.tika.exception.TikaException;
import org.apache.tika.exception.TikaTimeoutException;
import org.apache.tika.io.TemporaryResources;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ConcurrentLinkedDeque;
import java.util.stream.Collectors;
import java.util.stream.Stream;

/**
 * Tesseract linked into the process, as the static build links it: an engine per model, loaded
 * once and reused by one read at a time, so that reading an image starts no process and loads no
 * model. A read runs as the tesseract program runs it, rendering into a file. Detection reports
 * what the program reports, without the page segmentation the program runs first and then
 * discards.
 */
final class LinkedTesseract implements Tesseract {

    private static final int SUCCEEDED = 0;
    private static final int TIMED_OUT = 2;
    private static final String DEFAULT_LANGUAGE = "eng";
    private static final String OSD_LANGUAGE = "osd";
    private static final String TEXT_EXTENSION = ".txt";
    private static final String MODEL_EXTENSION = ".traineddata";

    private final Path tessdata;
    private final Set<String> languages;
    private final Map<String, ConcurrentLinkedDeque<Long>> idle = new ConcurrentHashMap<>();

    LinkedTesseract(Path tessdata) throws IOException {
        this.tessdata = tessdata;
        try (Stream<Path> files = Files.walk(tessdata)) {
            this.languages = files
                    .filter(file -> file.getFileName().toString().endsWith(MODEL_EXTENSION))
                    .map(file -> {
                        final String name = tessdata.relativize(file).toString()
                                .replace(file.getFileSystem().getSeparator(), "/");
                        return name.substring(0, name.length() - MODEL_EXTENSION.length());
                    })
                    .collect(Collectors.toUnmodifiableSet());
        }
    }

    @Override
    public Reading read(Path image, String pageSegMode, String language,
                        Map<String, String> variables, long timeoutMillis)
            throws IOException, TikaException {
        final String model = language == null || language.isBlank() ? DEFAULT_LANGUAGE : language;
        final byte[][] names = new byte[variables.size()][];
        final byte[][] values = new byte[variables.size()][];
        int index = 0;
        for (final Map.Entry<String, String> variable : variables.entrySet()) {
            names[index] = utf8(variable.getKey());
            values[index] = utf8(variable.getValue());
            index++;
        }
        final long engine = take(model);
        try (TemporaryResources temporary = new TemporaryResources()) {
            final Path outputBase = temporary.createTempFile();
            final Path output =
                    outputBase.resolveSibling(outputBase.getFileName() + TEXT_EXTENSION);
            try {
                final int status = run(engine, utf8(image.toString()), utf8(outputBase.toString()),
                        Integer.parseInt(pageSegMode), names, values,
                        (int) Math.min(timeoutMillis, Integer.MAX_VALUE));
                if (status == TIMED_OUT) {
                    throw new TikaTimeoutException("Reading " + image + " failed: tesseract did "
                            + "not finish within " + timeoutMillis + " ms");
                }
                if (status != SUCCEEDED) {
                    throw new TikaException("Reading " + image + " failed: tesseract could not "
                            + "read it with " + model);
                }
                return new Reading(Files.readString(output, StandardCharsets.UTF_8),
                        meanConfidence(engine));
            } finally {
                Files.deleteIfExists(output);
            }
        } finally {
            release(model, engine);
        }
    }

    @Override
    public String detect(Path image, long timeoutMillis) throws IOException {
        final long engine;
        try {
            engine = take(OSD_LANGUAGE);
        } catch (TikaException e) {
            throw new IOException("Detecting the script of " + image + " failed", e);
        }
        try {
            final byte[] report = detect(engine, utf8(image.toString()));
            return report == null ? null : new String(report, StandardCharsets.UTF_8);
        } finally {
            release(OSD_LANGUAGE, engine);
        }
    }

    @Override
    public Set<String> languages() {
        return languages;
    }

    /** An idle engine for a model, loaded when every engine for it is in use. */
    private long take(String language) throws TikaException {
        final Long engine = idle.computeIfAbsent(language, model -> new ConcurrentLinkedDeque<>())
                .poll();
        if (engine != null) {
            return engine;
        }
        final long created = create(utf8(tessdata.toString()), utf8(language));
        if (created == 0) {
            throw new TikaException("tesseract cannot load the model " + language + " from "
                    + tessdata);
        }
        return created;
    }

    private void release(String language, long engine) {
        idle.get(language).push(engine);
    }

    private static byte[] utf8(String text) {
        return text.getBytes(StandardCharsets.UTF_8);
    }

    /** Loads an engine for a model, or returns 0 when it cannot. */
    private static native long create(byte[] tessdata, byte[] language);

    /** Reads an image with an engine and tells whether it succeeded, failed or timed out. */
    private static native int run(long engine, byte[] image, byte[] outputBase, int pageSegMode,
                                  byte[][] names, byte[][] values, int timeoutMillis);

    /** The mean confidence of the words an engine read last, as tesseract's MeanTextConf. */
    private static native int meanConfidence(long engine);

    /** The orientation and script report of an osd engine on an image, or null when it has none. */
    private static native byte[] detect(long engine, byte[] image);
}
