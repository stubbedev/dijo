package ai.yobix;

import org.apache.tika.detect.EncodingDetector;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.parser.txt.CharsetDetector;
import org.apache.tika.parser.txt.CharsetMatch;

import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.Charset;
import java.util.Set;

/**
 * Recognises the legacy encodings that juniversalchardet, the first statistical detector Tika
 * asks, has no model for: Central European, Turkish and Arabic text. It takes them for something
 * it knows, such as windows-1252 or MacCyrillic, and garbles the text. ICU's detector models
 * them, so its best match is taken when it is one of these encodings and reasonably confident;
 * anything else is left to the detectors that follow.
 */
final class UnmodelledEncodingDetector implements EncodingDetector {

    private static final Set<String> UNMODELLED = Set.of(
            "windows-1250", "ISO-8859-2", "windows-1254", "ISO-8859-9", "windows-1256",
            "ISO-8859-6");
    private static final int MIN_CONFIDENCE = 30;

    @Override
    public Charset detect(InputStream input, Metadata metadata) throws IOException {
        if (input == null) {
            return null;
        }
        final CharsetDetector detector = new CharsetDetector();
        detector.setText(Utf8EncodingDetector.prefix(input));
        final CharsetMatch best = detector.detect();
        if (best == null || !UNMODELLED.contains(best.getName())
                || best.getConfidence() < MIN_CONFIDENCE) {
            return null;
        }
        return Charset.forName(best.getName());
    }
}
