package ai.yobix;

import org.apache.tika.exception.TikaException;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.metadata.TikaCoreProperties;
import org.apache.tika.mime.MediaType;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.sax.XHTMLContentHandler;
import org.apache.tika.utils.XMLReaderUtils;
import org.xml.sax.Attributes;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;
import org.xml.sax.helpers.DefaultHandler;

import java.io.IOException;
import java.io.InputStream;
import java.util.ArrayDeque;
import java.util.ArrayList;
import java.util.Deque;
import java.util.List;
import java.util.Set;

/**
 * FictionBook (FB2) books: the book's title, authors and language as metadata, its sections as
 * headings and paragraphs, verse and quotations as paragraphs, and no embedded binaries. Tika
 * reads FB2 as generic XML, running paragraphs together and keeping the names of embedded
 * images and the language code in the text.
 */
final class FictionBookParser implements Parser {

    static final MediaType FICTION_BOOK = MediaType.application("x-fictionbook+xml");

    private static final int MAX_HEADING_LEVEL = 6;
    private static final Set<String> PARAGRAPHS = Set.of("p", "v", "text-author", "subtitle");
    private static final Set<String> NAME_PARTS =
            Set.of("first-name", "middle-name", "last-name", "nickname");

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return Set.of(FICTION_BOOK);
    }

    @Override
    public void parse(TikaInputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        final Book book = new Book(metadata);
        XMLReaderUtils.parseSAX(stream, book, context);
        final XHTMLContentHandler xhtml = new XHTMLContentHandler(handler, metadata);
        xhtml.startDocument();
        for (final Block block : book.blocks) {
            xhtml.element(block.element(), block.text());
        }
        xhtml.endDocument();
    }

    /** A heading or a paragraph of the book. */
    private record Block(String element, String text) {
    }

    /** Collects the book's metadata and blocks while the XML is read. */
    private static final class Book extends DefaultHandler {
        private final Metadata metadata;
        private final List<Block> blocks = new ArrayList<>();
        private final Deque<String> open = new ArrayDeque<>();
        private final StringBuilder text = new StringBuilder();
        private final List<String> name = new ArrayList<>();
        private int sections;
        private String collecting;

        Book(Metadata metadata) {
            this.metadata = metadata;
        }

        private boolean inside(String element) {
            return open.contains(element);
        }

        @Override
        public void startElement(String uri, String localName, String qName, Attributes atts) {
            if ("section".equals(localName)) {
                sections++;
            }
            open.push(localName);
            if (inside("binary") || collecting != null) {
                return;
            }
            final boolean described = inside("title-info");
            if (described && ("book-title".equals(localName) || "lang".equals(localName)
                    || NAME_PARTS.contains(localName))) {
                collecting = localName;
            } else if (inside("body") && ("title".equals(localName)
                    || PARAGRAPHS.contains(localName))) {
                collecting = localName;
            }
            text.setLength(0);
        }

        @Override
        public void characters(char[] ch, int start, int length) {
            if (collecting != null && !inside("binary")) {
                text.append(ch, start, length);
            }
        }

        @Override
        public void endElement(String uri, String localName, String qName) {
            open.pop();
            if ("section".equals(localName)) {
                sections--;
            }
            if ("p".equals(localName) && "title".equals(collecting)) {
                text.append(' ');
                return;
            }
            if ("author".equals(localName) && inside("title-info") && !name.isEmpty()) {
                metadata.add(TikaCoreProperties.CREATOR, String.join(" ", name));
                name.clear();
            }
            if (!localName.equals(collecting)) {
                return;
            }
            collecting = null;
            final String value = text.toString().replaceAll("\\s+", " ").strip();
            if (value.isEmpty()) {
                return;
            }
            switch (localName) {
                case "book-title" -> metadata.set(TikaCoreProperties.TITLE, value);
                case "lang" -> metadata.set(TikaCoreProperties.LANGUAGE, value);
                case "title" -> blocks.add(new Block(
                        "h" + Math.min(MAX_HEADING_LEVEL, sections + 1), value));
                case "subtitle" -> blocks.add(new Block(
                        "h" + Math.min(MAX_HEADING_LEVEL, sections + 2), value));
                default -> {
                    if (NAME_PARTS.contains(localName)) {
                        name.add(value);
                    } else {
                        blocks.add(new Block("p", value));
                    }
                }
            }
        }
    }
}
