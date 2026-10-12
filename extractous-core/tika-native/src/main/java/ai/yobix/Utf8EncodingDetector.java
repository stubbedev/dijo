package ai.yobix;

import org.apache.tika.detect.EncodingDetector;
import org.apache.tika.metadata.Metadata;

import java.io.IOException;
import java.io.InputStream;
import java.nio.ByteBuffer;
import java.nio.CharBuffer;
import java.nio.charset.Charset;
import java.nio.charset.CharsetDecoder;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;

/**
 * Recognises UTF-8 by validating it. Text in a legacy single-byte encoding almost never forms
 * valid multi-byte UTF-8 sequences, while the statistical detectors that follow misjudge UTF-8
 * text with only a few non-ASCII characters. Text that is pure ASCII, or not valid UTF-8, is left
 * to them.
 */
final class Utf8EncodingDetector implements EncodingDetector {

    /** How much of the text the detectors look at. */
    static final int READ_LIMIT = 64 * 1024;

    @Override
    public Charset detect(InputStream input, Metadata metadata) throws IOException {
        if (input == null) {
            return null;
        }
        final byte[] prefix = prefix(input);
        return isUtf8WithNonAscii(prefix, prefix.length < READ_LIMIT) ? StandardCharsets.UTF_8 : null;
    }

    /** The first {@link #READ_LIMIT} bytes of the text, leaving the stream where it was. */
    static byte[] prefix(InputStream input) throws IOException {
        input.mark(READ_LIMIT);
        try {
            return input.readNBytes(READ_LIMIT);
        } finally {
            input.reset();
        }
    }

    /**
     * @param complete whether the bytes are the whole text, rather than a prefix that may end
     *                 inside a character
     */
    static boolean isUtf8WithNonAscii(byte[] bytes, boolean complete) {
        boolean nonAscii = false;
        for (final byte b : bytes) {
            if (b < 0) {
                nonAscii = true;
                break;
            }
        }
        if (!nonAscii) {
            return false;
        }
        final CharsetDecoder decoder = StandardCharsets.UTF_8.newDecoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT);
        return !decoder.decode(ByteBuffer.wrap(bytes), CharBuffer.allocate(bytes.length), complete)
                .isError();
    }
}
