package ai.yobix;

import org.apache.commons.io.input.BoundedInputStream;
import org.apache.tika.exception.TikaException;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.mime.MediaType;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.Set;

/**
 * Apple Mail messages (.emlx): a line with the message's length in bytes, the message itself,
 * then a property list of Mail's flags. The message is parsed as the email it is, headers and
 * attachments included; Tika reads the whole file as plain text, length and property list
 * included.
 */
final class EmlxParser implements Parser {

    static final MediaType EMLX = MediaType.parse("message/x-emlx");

    private static final int MAX_LENGTH_DIGITS = 19;

    private final Parser message;

    /** @param message parses an RFC 822 message */
    EmlxParser(Parser message) {
        this.message = message;
    }

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return Set.of(EMLX);
    }

    @Override
    public void parse(TikaInputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        final long length = messageLength(stream);
        try (TikaInputStream body = TikaInputStream.get(BoundedInputStream.builder()
                .setInputStream(stream).setMaxCount(length).setPropagateClose(false).get())) {
            message.parse(body, handler, metadata, context);
        }
    }

    /** Reads the first line, the decimal length of the message that follows it. */
    private static long messageLength(InputStream stream) throws IOException, TikaException {
        final ByteArrayOutputStream digits = new ByteArrayOutputStream();
        int next;
        while ((next = stream.read()) != '\n') {
            if (next == -1 || digits.size() > MAX_LENGTH_DIGITS) {
                throw new TikaException("The .emlx file does not start with the message's length");
            }
            if (next != '\r' && next != ' ') {
                digits.write(next);
            }
        }
        try {
            return Long.parseLong(digits.toString(StandardCharsets.US_ASCII));
        } catch (NumberFormatException e) {
            throw new TikaException("The .emlx file does not start with the message's length", e);
        }
    }
}
