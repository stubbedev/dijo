package ai.yobix;

import org.apache.tika.sax.ContentHandlerDecorator;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import java.util.Set;

/**
 * Ends every block-level element with a line break in plain-text output, unless the text already
 * ends with one. Most parsers write their own line breaks, but some (OpenDocument headings, for
 * one) leave them to the markup, so a heading and the paragraph after it ran together.
 */
final class BlockBreakHandler extends ContentHandlerDecorator {

    private static final Set<String> BLOCKS = Set.of(
            "p", "h1", "h2", "h3", "h4", "h5", "h6", "li", "div", "tr", "table", "blockquote",
            "pre", "dt", "dd", "title");
    private static final char[] LINE_BREAK = {'\n'};

    private boolean atLineStart = true;

    BlockBreakHandler(ContentHandler handler) {
        super(handler);
    }

    @Override
    public void characters(char[] characters, int start, int length) throws SAXException {
        if (length > 0) {
            atLineStart = characters[start + length - 1] == '\n';
        }
        super.characters(characters, start, length);
    }

    @Override
    public void ignorableWhitespace(char[] characters, int start, int length)
            throws SAXException {
        if (length > 0) {
            atLineStart = characters[start + length - 1] == '\n';
        }
        super.ignorableWhitespace(characters, start, length);
    }

    @Override
    public void endElement(String uri, String localName, String name) throws SAXException {
        super.endElement(uri, localName, name);
        if (!atLineStart && BLOCKS.contains(localName)) {
            super.characters(LINE_BREAK, 0, 1);
            atLineStart = true;
        }
    }
}
