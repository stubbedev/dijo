package ai.yobix;

import org.apache.tika.sax.ContentHandlerDecorator;
import org.xml.sax.Attributes;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import java.util.ArrayDeque;
import java.util.Deque;
import java.util.Set;

/**
 * Ends every block-level element with a line break in plain-text output, unless the text already
 * ends with one. Most parsers write their own line breaks, but some (OpenDocument headings, for
 * one) leave them to the markup, so a heading and the paragraph after it ran together. Within a
 * table cell, line breaks go only between the cell's paragraphs: Tika ends each paragraph with
 * one, which put every cell of a row whose cells hold paragraphs on a line of its own.
 */
final class BlockBreakHandler extends ContentHandlerDecorator {

    static final Set<String> BLOCKS = Set.of(
            "p", "h1", "h2", "h3", "h4", "h5", "h6", "li", "div", "tr", "table", "blockquote",
            "pre", "dt", "dd", "title");
    private static final Set<String> CELLS = Set.of("td", "th");
    private static final char[] LINE_BREAK = {'\n'};

    /** Whether an open cell has text yet, and whether a block in it has ended since. */
    private static final class Cell {
        boolean hasText;
        boolean lineEnded;
    }

    private final Deque<Cell> cells = new ArrayDeque<>();
    private boolean atLineStart = true;

    BlockBreakHandler(ContentHandler handler) {
        super(handler);
    }

    @Override
    public void startElement(String uri, String localName, String name, Attributes atts)
            throws SAXException {
        super.startElement(uri, localName, name, atts);
        if (CELLS.contains(localName)) {
            cells.push(new Cell());
        } else if (BLOCKS.contains(localName)) {
            endCellLine();
        }
    }

    @Override
    public void characters(char[] characters, int start, int length) throws SAXException {
        if (keepsInCell(characters, start, length)) {
            track(characters, start, length);
            super.characters(characters, start, length);
        }
    }

    @Override
    public void ignorableWhitespace(char[] characters, int start, int length)
            throws SAXException {
        if (keepsInCell(characters, start, length)) {
            track(characters, start, length);
            super.ignorableWhitespace(characters, start, length);
        }
    }

    @Override
    public void endElement(String uri, String localName, String name) throws SAXException {
        super.endElement(uri, localName, name);
        if (CELLS.contains(localName)) {
            cells.pop();
            return;
        }
        if (!BLOCKS.contains(localName)) {
            return;
        }
        if (!cells.isEmpty()) {
            endCellLine();
            return;
        }
        if (!atLineStart) {
            super.characters(LINE_BREAK, 0, 1);
            atLineStart = true;
        }
    }

    private void endCellLine() {
        final Cell cell = cells.peek();
        if (cell != null) {
            cell.lineEnded |= cell.hasText;
        }
    }

    /**
     * Whether text in the open cell, if any, is written: whitespace before the cell's first text
     * or after one of its blocks is not, and text after one of its blocks starts a new line.
     */
    private boolean keepsInCell(char[] characters, int start, int length) throws SAXException {
        final Cell cell = cells.peek();
        if (cell == null) {
            return true;
        }
        final boolean blank = new String(characters, start, length).isBlank();
        if (blank && (!cell.hasText || cell.lineEnded)) {
            return false;
        }
        if (cell.lineEnded) {
            super.characters(LINE_BREAK, 0, 1);
            atLineStart = true;
            cell.lineEnded = false;
        }
        cell.hasText = true;
        return true;
    }

    private void track(char[] characters, int start, int length) {
        if (length > 0) {
            atLineStart = characters[start + length - 1] == '\n';
        }
    }
}
