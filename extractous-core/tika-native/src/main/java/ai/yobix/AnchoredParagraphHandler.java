package ai.yobix;

import org.apache.tika.sax.ContentHandlerDecorator;
import org.xml.sax.Attributes;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;
import org.xml.sax.helpers.AttributesImpl;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

/**
 * Writes a paragraph opened inside another after the one it is anchored in. OpenDocument
 * comments and notes come that way, as a paragraph in the middle of the sentence they are
 * anchored to, which cut the sentence in two. A paragraph held back longer than
 * {@link #MAX_HELD_CHARACTERS} is written where it stands, so memory stays bounded.
 */
final class AnchoredParagraphHandler extends ContentHandlerDecorator {

    private static final String PARAGRAPH = "p";
    private static final int MAX_HELD_CHARACTERS = 1 << 20;

    private interface Event {
        void replay() throws SAXException;
    }

    private final List<Event> held = new ArrayList<>();
    private int heldCharacters;
    private int paragraphs;
    private int holding;

    AnchoredParagraphHandler(ContentHandler handler) {
        super(handler);
    }

    @Override
    public void startElement(String uri, String localName, String name, Attributes atts)
            throws SAXException {
        if (PARAGRAPH.equals(localName)) {
            paragraphs++;
            if (paragraphs > 1 && holding == 0) {
                holding = paragraphs;
            }
        }
        if (holding == 0) {
            super.startElement(uri, localName, name, atts);
            return;
        }
        final Attributes copy = new AttributesImpl(atts);
        held.add(() -> AnchoredParagraphHandler.super.startElement(uri, localName, name, copy));
    }

    @Override
    public void endElement(String uri, String localName, String name) throws SAXException {
        if (holding != 0) {
            held.add(() -> AnchoredParagraphHandler.super.endElement(uri, localName, name));
        } else {
            super.endElement(uri, localName, name);
        }
        if (!PARAGRAPH.equals(localName)) {
            return;
        }
        if (paragraphs == holding) {
            holding = 0;
        }
        paragraphs--;
        if (paragraphs == 0) {
            release();
        }
    }

    @Override
    public void characters(char[] characters, int start, int length) throws SAXException {
        if (holding == 0) {
            super.characters(characters, start, length);
            return;
        }
        final char[] copy = Arrays.copyOfRange(characters, start, start + length);
        held.add(() -> AnchoredParagraphHandler.super.characters(copy, 0, copy.length));
        heldCharacters += length;
        if (heldCharacters > MAX_HELD_CHARACTERS) {
            release();
            holding = 0;
        }
    }

    @Override
    public void ignorableWhitespace(char[] characters, int start, int length)
            throws SAXException {
        if (holding == 0) {
            super.ignorableWhitespace(characters, start, length);
            return;
        }
        final char[] copy = Arrays.copyOfRange(characters, start, start + length);
        held.add(() -> AnchoredParagraphHandler.super.ignorableWhitespace(copy, 0, copy.length));
    }

    @Override
    public void endDocument() throws SAXException {
        release();
        super.endDocument();
    }

    private void release() throws SAXException {
        for (final Event event : held) {
            event.replay();
        }
        held.clear();
        heldCharacters = 0;
    }
}
