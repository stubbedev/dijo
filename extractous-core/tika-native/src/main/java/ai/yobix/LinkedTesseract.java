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
import java.util.concurrent.TimeUnit;
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
    private static final long IDLE_NANOS = TimeUnit.MINUTES.toNanos(5);
    private static final String MODEL_EXTENSION = ".traineddata";

    private final Path tessdata;
    private final Set<String> languages;
    private final Map<String, ConcurrentLinkedDeque<Idle>> idle = new ConcurrentHashMap<>();

    /** An engine no read is using, and since when, by {@link System#nanoTime()}. */
    private record Idle(long engine, long since) {
    }

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
    public Reading read(OcrImage image, String pageSegMode, String language,
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
        final int segmentation = Integer.parseInt(pageSegMode);
        final int timeout = (int) Math.min(timeoutMillis, Integer.MAX_VALUE);
        final long engine = take(model);
        try {
            final byte[][] text = new byte[1][];
            final int status = switch (image) {
                case OcrImage.File file -> runFile(engine, file.path(), segmentation, names,
                        values, timeout, text);
                case OcrImage.Pixels pixels when pixels.gray() -> runGray(engine,
                        pixels.grayLevels(), pixels.image().getWidth(),
                        pixels.image().getHeight(), segmentation, names, values, timeout, text);
                case OcrImage.Pixels pixels -> runRgb(engine, pixels.rgb(),
                        pixels.image().getWidth(), pixels.image().getHeight(), segmentation,
                        names, values, timeout, text);
            };
            if (status == TIMED_OUT) {
                throw new TikaTimeoutException("Reading " + describe(image) + " failed: "
                        + "tesseract did not finish within " + timeoutMillis + " ms");
            }
            if (status != SUCCEEDED) {
                throw new TikaException("Reading " + describe(image) + " failed: tesseract "
                        + "could not read it with " + model);
            }
            return new Reading(new String(text[0], StandardCharsets.UTF_8),
                    meanConfidence(engine));
        } finally {
            release(model, engine);
        }
    }

    /**
     * Reads an image file as the tesseract program does, rendering its text into a file, and
     * hands the text over.
     */
    private static int runFile(long engine, Path image, int pageSegMode, byte[][] names,
                               byte[][] values, int timeoutMillis, byte[][] text)
            throws IOException {
        try (TemporaryResources temporary = new TemporaryResources()) {
            final Path outputBase = temporary.createTempFile();
            final Path output =
                    outputBase.resolveSibling(outputBase.getFileName() + TEXT_EXTENSION);
            try {
                final int status = run(engine, utf8(image.toString()), utf8(outputBase.toString()),
                        pageSegMode, names, values, timeoutMillis);
                if (status == SUCCEEDED) {
                    text[0] = Files.readAllBytes(output);
                }
                return status;
            } finally {
                Files.deleteIfExists(output);
            }
        }
    }

    @Override
    public String detect(OcrImage image, long timeoutMillis) throws IOException {
        final long engine;
        try {
            engine = take(OSD_LANGUAGE);
        } catch (TikaException e) {
            throw new IOException("Detecting the script of " + describe(image) + " failed", e);
        }
        try {
            final byte[] report = switch (image) {
                case OcrImage.File file -> detect(engine, utf8(file.path().toString()));
                case OcrImage.Pixels pixels when pixels.gray() -> detectGray(engine,
                        pixels.grayLevels(), pixels.image().getWidth(),
                        pixels.image().getHeight());
                case OcrImage.Pixels pixels -> detectRgb(engine, pixels.rgb(),
                        pixels.image().getWidth(), pixels.image().getHeight());
            };
            return report == null ? null : new String(report, StandardCharsets.UTF_8);
        } finally {
            release(OSD_LANGUAGE, engine);
        }
    }

    private static String describe(OcrImage image) {
        return switch (image) {
            case OcrImage.File file -> file.path().toString();
            case OcrImage.Pixels pixels -> "a " + pixels.image().getWidth() + "x"
                    + pixels.image().getHeight() + " image";
        };
    }

    @Override
    public Set<String> languages() {
        return languages;
    }

    /**
     * An idle engine for a model, the one used last, loaded when every engine for it is in use.
     * Engines idle for longer than {@link #IDLE_NANOS} are released first, so a long-running
     * process does not keep a model it read with once in memory.
     */
    private long take(String language) throws TikaException {
        releaseIdle(System.nanoTime() - IDLE_NANOS);
        final Idle engine = idle.computeIfAbsent(language, model -> new ConcurrentLinkedDeque<>())
                .pollFirst();
        if (engine != null) {
            return engine.engine();
        }
        final long created = create(utf8(tessdata.toString()), utf8(language));
        if (created == 0) {
            throw new TikaException("tesseract cannot load the model " + language + " from "
                    + tessdata);
        }
        return created;
    }

    private void release(String language, long engine) {
        idle.get(language).addFirst(new Idle(engine, System.nanoTime()));
    }

    /** Destroys the engines idle since before a moment, the longest idle of each model first. */
    private void releaseIdle(long before) {
        for (final ConcurrentLinkedDeque<Idle> engines : idle.values()) {
            for (Idle oldest = engines.peekLast(); oldest != null && oldest.since() - before < 0;
                 oldest = engines.peekLast()) {
                if (engines.removeLastOccurrence(oldest)) {
                    destroy(oldest.engine());
                }
            }
        }
    }

    private static byte[] utf8(String text) {
        return text.getBytes(StandardCharsets.UTF_8);
    }

    /** Frees an engine and the model it loaded. */
    private static native void destroy(long engine);

    /** Loads an engine for a model, or returns 0 when it cannot. */
    private static native long create(byte[] tessdata, byte[] language);

    /** Reads an image with an engine and tells whether it succeeded, failed or timed out. */
    private static native int run(long engine, byte[] image, byte[] outputBase, int pageSegMode,
                                  byte[][] names, byte[][] values, int timeoutMillis);

    /**
     * Reads grey pixels, one byte each, row by row, as the tesseract program reads one page,
     * puts the text into text[0] and tells whether it succeeded, failed or timed out.
     */
    private static native int runGray(long engine, byte[] gray, int width, int height,
                                      int pageSegMode, byte[][] names, byte[][] values,
                                      int timeoutMillis, byte[][] text);

    /** Reads pixels of packed RGB as {@link #runGray} reads grey ones. */
    private static native int runRgb(long engine, int[] rgb, int width, int height,
                                     int pageSegMode, byte[][] names, byte[][] values,
                                     int timeoutMillis, byte[][] text);

    /** The mean confidence of the words an engine read last, as tesseract's MeanTextConf. */
    private static native int meanConfidence(long engine);

    /** The orientation and script report of an osd engine on an image, or null when it has none. */
    private static native byte[] detect(long engine, byte[] image);

    /** The orientation and script report of an osd engine on grey pixels, or null. */
    private static native byte[] detectGray(long engine, byte[] gray, int width, int height);

    /** The orientation and script report of an osd engine on RGB pixels, or null. */
    private static native byte[] detectRgb(long engine, int[] rgb, int width, int height);
}
