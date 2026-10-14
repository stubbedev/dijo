package ai.yobix;

import org.apache.tika.exception.TikaException;

import java.io.IOException;
import java.util.Map;
import java.util.Optional;
import java.util.Set;

/** Tesseract, as the build provides it. */
interface Tesseract {

    /**
     * What tesseract read in an image.
     *
     * @param text       the text, as {@code tesseract <image> stdout ... txt} prints it
     * @param confidence the mean confidence of its words, from 0 to 100, as tesseract's
     *                   {@code MeanTextConf} averages them: 0 when it found no words
     */
    record Reading(String text, int confidence) {
    }

    /**
     * A reading going badly: once {@code afterWords} of its words are read, their mean
     * confidence, as {@code MeanTextConf} takes it, is below {@code belowConfidence}.
     */
    record Poor(int afterWords, int belowConfidence) {
    }

    /**
     * Reads the text in an image, as {@code tesseract <image> stdout --psm <pageSegMode>
     * -l <language> -c <name>=<value>... txt} prints it, with its words' mean confidence.
     *
     * @throws org.apache.tika.exception.TikaTimeoutException when it takes longer than
     *                                                        timeoutMillis
     * @throws TikaException                                  when tesseract fails
     */
    Reading read(OcrImage image, String pageSegMode, String language,
                 Map<String, String> variables, long timeoutMillis)
            throws IOException, TikaException;

    /**
     * Reads as {@link #read} does, but gives up a reading that goes {@link Poor poorly}, as far
     * as the engine can tell while it reads: empty when it gave up. An engine that cannot tell
     * reads to the end.
     */
    default Optional<Reading> readUnlessPoor(OcrImage image, String pageSegMode, String language,
                                             Map<String, String> variables, Poor poor,
                                             long timeoutMillis)
            throws IOException, TikaException {
        return Optional.of(read(image, pageSegMode, language, variables, timeoutMillis));
    }

    /**
     * Detects the orientation and script of an image, as {@code tesseract <image> stdout --psm 0
     * -c <name>=<value>...} prints them, or null when tesseract cannot judge the image, as when
     * it has too little text, or takes longer than timeoutMillis.
     */
    String detect(OcrImage image, Map<String, String> variables, long timeoutMillis)
            throws IOException;

    /**
     * The models it reads with, named as {@code -l} names them, with a slash before a model in a
     * subdirectory on every platform: {@code eng}, {@code script/Latin}.
     */
    Set<String> languages() throws IOException;
}
