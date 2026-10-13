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
 * Writes a block opened in the middle of a paragraph after that paragraph. OpenDocument comments
 * and notes come that way, as a paragraph inside the paragraph they are anchored in, and so do
 * the footnote references of Word documents, as an empty block: either cut the sentence around
 * its anchor in two, as does the line break Tika writes after a block, which goes with it. A
 * block opened before its paragraph has any text cuts nothing and stays where it is. One held
 * back longer than {@link #MAX_HELD_CHARACTERS} is written where it stands, so memory stays
 * bounded.
 */
final class AnchoredBlockHandler extends ContentHandlerDecorator {

    private static final String PARAGRAPH = "p";
    private static final int MAX_HELD_CHARACTERS = 1 << 20;

    private interface Event {
        void replay() throws SAXException;
    }

    private final List<Event> held = new ArrayList<>();
    private int heldCharacters;
    private int depth;
    private int paragraphDepth;
    private boolean paragraphHasText;
    private int holdingDepth;
    private boolean afterHeld;

    AnchoredBlockHandler(ContentHandler handler) {
        super(handler);
    }

    @Override
    public void startElement(String uri, String localName, String name, Attributes atts)
            throws SAXException {
        depth++;
        afterHeld = false;
        if (holdingDepth == 0 && paragraphDepth != 0 && paragraphHasText
                && BlockBreakHandler.BLOCKS.contains(localName)) {
            holdingDepth = depth;
        }
        if (holdingDepth != 0) {
            final Attributes copy = new AttributesImpl(atts);
            held.add(() -> AnchoredBlockHandler.super.startElement(uri, localName, name, copy));
            return;
        }
        super.startElement(uri, localName, name, atts);
        if (paragraphDepth == 0 && PARAGRAPH.equals(localName)) {
            paragraphDepth = depth;
            paragraphHasText = false;
        }
    }

    @Override
    public void endElement(String uri, String localName, String name) throws SAXException {
        if (holdingDepth != 0) {
            held.add(() -> AnchoredBlockHandler.super.endElement(uri, localName, name));
            if (depth == holdingDepth) {
                holdingDepth = 0;
                afterHeld = true;
            }
        } else {
            afterHeld = false;
            super.endElement(uri, localName, name);
            if (depth == paragraphDepth) {
                paragraphDepth = 0;
                release();
            }
        }
        depth--;
    }

    @Override
    public void characters(char[] characters, int start, int length) throws SAXException {
        final boolean blank = new String(characters, start, length).isBlank();
        afterHeld &= blank;
        if (holdingDepth == 0 && !afterHeld) {
            super.characters(characters, start, length);
            paragraphHasText |= paragraphDepth != 0 && !blank;
            return;
        }
        final char[] copy = Arrays.copyOfRange(characters, start, start + length);
        held.add(() -> AnchoredBlockHandler.super.characters(copy, 0, copy.length));
        heldCharacters += length;
        if (heldCharacters > MAX_HELD_CHARACTERS) {
            release();
            holdingDepth = 0;
            afterHeld = false;
        }
    }

    @Override
    public void ignorableWhitespace(char[] characters, int start, int length)
            throws SAXException {
        if (holdingDepth == 0 && !afterHeld) {
            super.ignorableWhitespace(characters, start, length);
            return;
        }
        final char[] copy = Arrays.copyOfRange(characters, start, start + length);
        held.add(() -> AnchoredBlockHandler.super.ignorableWhitespace(copy, 0, copy.length));
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
