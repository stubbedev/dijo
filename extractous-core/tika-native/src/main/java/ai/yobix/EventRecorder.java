package ai.yobix;

import org.xml.sax.Attributes;
import org.xml.sax.helpers.DefaultHandler;

import java.io.ByteArrayOutputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.io.UncheckedIOException;
import java.nio.charset.StandardCharsets;

/**
 * Records the SAX events an embedded document's parser writes, for the Rust side to replay
 * through its own handlers with every chunk of text as the parser wrote it. Each event is a tag
 * byte followed by its strings, each string its length in UTF-8 bytes as an int and the bytes:
 * {@code S} a start tag (local name, attribute count, then each attribute's local name and
 * value), {@code E} an end tag (local name), {@code C} characters and {@code W} ignorable
 * whitespace (the text).
 */
final class EventRecorder extends DefaultHandler {

    private final ByteArrayOutputStream bytes = new ByteArrayOutputStream();
    private final DataOutputStream out = new DataOutputStream(bytes);

    byte[] events() {
        return bytes.toByteArray();
    }

    @Override
    public void startElement(String uri, String localName, String name, Attributes atts) {
        tag('S');
        string(localName);
        integer(atts.getLength());
        for (int i = 0; i < atts.getLength(); i++) {
            string(atts.getLocalName(i));
            string(atts.getValue(i));
        }
    }

    @Override
    public void endElement(String uri, String localName, String name) {
        tag('E');
        string(localName);
    }

    @Override
    public void characters(char[] characters, int start, int length) {
        tag('C');
        string(new String(characters, start, length));
    }

    @Override
    public void ignorableWhitespace(char[] characters, int start, int length) {
        tag('W');
        string(new String(characters, start, length));
    }

    private void tag(char tag) {
        try {
            out.writeByte(tag);
        } catch (IOException e) {
            throw new UncheckedIOException(e);
        }
    }

    private void integer(int value) {
        try {
            out.writeInt(value);
        } catch (IOException e) {
            throw new UncheckedIOException(e);
        }
    }

    private void string(String value) {
        final byte[] utf8 = value.getBytes(StandardCharsets.UTF_8);
        integer(utf8.length);
        try {
            out.write(utf8);
        } catch (IOException e) {
            throw new UncheckedIOException(e);
        }
    }
}
