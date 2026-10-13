package ai.yobix;

import org.apache.tika.sax.ContentHandlerDecorator;
import org.xml.sax.Attributes;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import java.util.ArrayDeque;
import java.util.Deque;

/**
 * Keeps the elements passed on balanced, so XML written from them is well-formed: an end tag
 * without an open element of its name is dropped, one that skips open elements closes them
 * first, and the elements still open at the end of the document are closed. Tika unbalances
 * them when it stops an embedded document midway, as it does past its limit on nested
 * elements, and its XML writer then fails the whole parse.
 */
final class WellFormedContentHandler extends ContentHandlerDecorator {

    private record Open(String uri, String localName, String name) {
    }

    private final Deque<Open> open = new ArrayDeque<>();

    WellFormedContentHandler(ContentHandler handler) {
        super(handler);
    }

    @Override
    public void startElement(String uri, String localName, String name, Attributes atts)
            throws SAXException {
        super.startElement(uri, localName, name, atts);
        open.push(new Open(uri, localName, name));
    }

    @Override
    public void endElement(String uri, String localName, String name) throws SAXException {
        if (open.stream().noneMatch(element -> element.name().equals(name))) {
            return;
        }
        while (true) {
            final Open element = open.pop();
            super.endElement(element.uri(), element.localName(), element.name());
            if (element.name().equals(name)) {
                return;
            }
        }
    }

    @Override
    public void endDocument() throws SAXException {
        while (!open.isEmpty()) {
            final Open element = open.pop();
            super.endElement(element.uri(), element.localName(), element.name());
        }
        super.endDocument();
    }
}
