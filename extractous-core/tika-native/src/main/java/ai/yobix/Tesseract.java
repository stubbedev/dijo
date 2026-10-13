package ai.yobix;

import org.apache.tika.exception.TikaException;

import java.io.IOException;
import java.nio.file.Path;
import java.util.Map;
import java.util.Set;

/** Tesseract, as the build provides it. */
interface Tesseract {

    /**
     * Reads the text in an image, as {@code tesseract <image> stdout --psm <pageSegMode>
     * -l <language> -c <name>=<value>... txt} prints it.
     *
     * @throws org.apache.tika.exception.TikaTimeoutException when it takes longer than
     *                                                        timeoutMillis
     * @throws TikaException                                  when tesseract fails
     */
    String read(Path image, String pageSegMode, String language, Map<String, String> variables,
                long timeoutMillis) throws IOException, TikaException;

    /**
     * Detects the orientation and script of an image, as {@code tesseract <image> stdout --psm 0}
     * prints them, or null when tesseract cannot judge the image, as when it has too little
     * text, or takes longer than timeoutMillis.
     */
    String detect(Path image, long timeoutMillis) throws IOException;

    /** The models it reads with, named as {@code -l} names them: {@code eng}, {@code script/Latin}. */
    Set<String> languages() throws IOException;
}
