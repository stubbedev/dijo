package ai.yobix;

import org.apache.tika.exception.TikaException;
import org.apache.tika.exception.TikaTimeoutException;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Map;
import java.util.Optional;
import java.util.Set;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ConcurrentLinkedDeque;
import java.util.concurrent.TimeUnit;
import java.util.stream.Collectors;
import java.util.stream.Stream;

/**
 * Tesseract linked into the static executable with these native functions: an engine per model,
 * loaded once and reused by one read at a time, so that reading an image starts no process and
 * loads no model. A read recognizes each page as the tesseract program does and hands its text over as
 * the program's text renderer writes it. Detection reports what the program reports, without the
 * page segmentation the program runs first and then discards.
 */
final class LinkedTesseract implements Tesseract {

    private static final int SUCCEEDED = 0;
    private static final int TIMED_OUT = 2;
    private static final int GAVE_UP = 3;
    private static final Poor NEVER_POOR = new Poor(0, 0);
    private static final String DEFAULT_LANGUAGE = "eng";
    private static final String OSD_LANGUAGE = "osd";
    private static final long IDLE_NANOS = TimeUnit.MINUTES.toNanos(5);
    private static final String MODEL_EXTENSION = ".traineddata";

    private final Path tessdata;
    private final Set<String> languages;
    private final Map<String, ConcurrentLinkedDeque<Idle>> idle = new ConcurrentHashMap<>();

    /** An engine no read is using, and since when, by {@link System#nanoTime()}. */
    private record Idle(long engine, long since) {
    }

    /** Tesseract variables as the native functions take them: names and values in UTF-8. */
    private record Variables(byte[][] names, byte[][] values) {

        static Variables of(Map<String, String> variables) {
            final byte[][] names = new byte[variables.size()][];
            final byte[][] values = new byte[variables.size()][];
            int index = 0;
            for (final Map.Entry<String, String> variable : variables.entrySet()) {
                names[index] = utf8(variable.getKey());
                values[index] = utf8(variable.getValue());
                index++;
            }
            return new Variables(names, values);
        }
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
        return readUnlessPoor(image, pageSegMode, language, variables, NEVER_POOR, timeoutMillis)
                .orElseThrow(() -> new TikaException("Reading " + describe(image)
                        + " failed: tesseract gave up a reading it was to read to the end"));
    }

    @Override
    public Optional<Reading> readUnlessPoor(OcrImage image, String pageSegMode, String language,
                                            Map<String, String> variables, Poor poor,
                                            long timeoutMillis)
            throws IOException, TikaException {
        final String model = language == null || language.isBlank() ? DEFAULT_LANGUAGE : language;
        final Variables encoded = Variables.of(variables);
        final byte[][] names = encoded.names();
        final byte[][] values = encoded.values();
        final int segmentation = Integer.parseInt(pageSegMode);
        final int timeout = (int) Math.min(timeoutMillis, Integer.MAX_VALUE);
        final long engine = take(model);
        try (ReadingThreads threads = ReadingThreads.take()) {
            final byte[][] text = new byte[1][];
            final int status = switch (image) {
                case OcrImage.File file -> runFile(engine, utf8(file.path().toString()),
                        segmentation, names, values, poor.afterWords(), poor.belowConfidence(),
                        threads.threads(), timeout, text);
                case OcrImage.Pixels pixels when pixels.gray() -> runGray(engine,
                        pixels.grayLevels(), pixels.image().getWidth(),
                        pixels.image().getHeight(), segmentation, names, values,
                        poor.afterWords(), poor.belowConfidence(), threads.threads(), timeout,
                        text);
                case OcrImage.Pixels pixels -> runRgb(engine, pixels.rgb(),
                        pixels.image().getWidth(), pixels.image().getHeight(), segmentation,
                        names, values, poor.afterWords(), poor.belowConfidence(),
                        threads.threads(), timeout, text);
            };
            if (status == GAVE_UP) {
                return Optional.empty();
            }
            if (status == TIMED_OUT) {
                throw new TikaTimeoutException("Reading " + describe(image) + " failed: "
                        + "tesseract did not finish within " + timeoutMillis + " ms");
            }
            if (status != SUCCEEDED) {
                throw new TikaException("Reading " + describe(image) + " failed: tesseract "
                        + "could not read it with " + model);
            }
            return Optional.of(new Reading(new String(text[0], StandardCharsets.UTF_8),
                    meanConfidence(engine)));
        } finally {
            release(model, engine);
        }
    }

    @Override
    public String detect(OcrImage image, Map<String, String> variables, long timeoutMillis)
            throws IOException {
        final Variables encoded = Variables.of(variables);
        final long engine;
        try {
            engine = take(OSD_LANGUAGE);
        } catch (TikaException e) {
            throw new IOException("Detecting the script of " + describe(image) + " failed", e);
        }
        try {
            final byte[] report = switch (image) {
                case OcrImage.File file -> detect(engine, utf8(file.path().toString()),
                        encoded.names(), encoded.values());
                case OcrImage.Pixels pixels when pixels.gray() -> detectGray(engine,
                        pixels.grayLevels(), pixels.image().getWidth(),
                        pixels.image().getHeight(), encoded.names(), encoded.values());
                case OcrImage.Pixels pixels -> detectRgb(engine, pixels.rgb(),
                        pixels.image().getWidth(), pixels.image().getHeight(), encoded.names(),
                        encoded.values());
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

    /**
     * Reads an image file as the tesseract program does, every page of a TIFF, puts the text into
     * text[0] and tells whether it succeeded, failed, timed out or gave up, as {@link #runGray}
     * does.
     */
    private static native int runFile(long engine, byte[] image, int pageSegMode, byte[][] names,
                                      byte[][] values, int giveUpAfterWords,
                                      int giveUpBelowConfidence, int threads, int timeoutMillis,
                                      byte[][] text);

    /**
     * Reads grey pixels, one byte each, row by row, as the tesseract program reads one page,
     * puts the text into text[0] and tells whether it succeeded, failed, timed out or gave up:
     * with giveUpAfterWords above 0, it gives up once that many words are read with a mean
     * confidence below giveUpBelowConfidence. The recognizer divides its work among threads.
     */
    private static native int runGray(long engine, byte[] gray, int width, int height,
                                      int pageSegMode, byte[][] names, byte[][] values,
                                      int giveUpAfterWords, int giveUpBelowConfidence,
                                      int threads, int timeoutMillis, byte[][] text);

    /** Reads pixels of packed RGB as {@link #runGray} reads grey ones. */
    private static native int runRgb(long engine, int[] rgb, int width, int height,
                                     int pageSegMode, byte[][] names, byte[][] values,
                                     int giveUpAfterWords, int giveUpBelowConfidence,
                                     int threads, int timeoutMillis, byte[][] text);

    /** The mean confidence of the words an engine read last, as tesseract's MeanTextConf. */
    private static native int meanConfidence(long engine);

    /**
     * The orientation and script report of an osd engine on an image, with variables set for it,
     * or null when it has none.
     */
    private static native byte[] detect(long engine, byte[] image, byte[][] names,
                                        byte[][] values);

    /** The orientation and script report of an osd engine on grey pixels, or null. */
    private static native byte[] detectGray(long engine, byte[] gray, int width, int height,
                                            byte[][] names, byte[][] values);

    /** The orientation and script report of an osd engine on RGB pixels, or null. */
    private static native byte[] detectRgb(long engine, int[] rgb, int width, int height,
                                           byte[][] names, byte[][] values);
}
