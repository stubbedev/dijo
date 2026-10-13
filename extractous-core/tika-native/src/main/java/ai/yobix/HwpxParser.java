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
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;

/**
 * Hancom Office documents in HWPX, the XML format Hangul writes since 2010 (KS X 6101): the
 * sections the package lists, in their order, as paragraphs and tables, footnotes where they
 * are anchored, and the title, language, author and dates of the package as metadata. Running
 * headers and footers are page furniture and left out. Tika reads an HWPX file as a ZIP archive,
 * listing its files and running all their XML text together.
 */
final class HwpxParser implements Parser {

    static final MediaType HWPX = MediaType.application("hwp+zip");

    private static final String CONTAINER = "META-INF/container.xml";
    private static final Set<String> FURNITURE = Set.of("header", "footer", "masterPage");
    private static final Set<String> NOTES = Set.of("footNote", "endNote");

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return Set.of(HWPX);
    }

    @Override
    public void parse(TikaInputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        final TikaInputStream input = TikaInputStream.get(stream);
        try (ZipFile zip = new ZipFile(input.getFile())) {
            final Package pack = new Package(metadata);
            parsePart(zip, packagePath(zip, context), pack, context);
            final XHTMLContentHandler xhtml = new XHTMLContentHandler(handler, metadata);
            xhtml.startDocument();
            for (final String idref : pack.spine) {
                final String href = pack.manifest.get(idref);
                if (href == null) {
                    throw new TikaException("The HWPX spine names the missing item " + idref);
                }
                parsePart(zip, href, new Section(xhtml), context);
            }
            xhtml.endDocument();
        }
    }

    /** Where the package file is, as META-INF/container.xml names it. */
    private static String packagePath(ZipFile zip, ParseContext context)
            throws IOException, SAXException, TikaException {
        final List<String> rootFiles = new ArrayList<>();
        parsePart(zip, CONTAINER, new DefaultHandler() {
            @Override
            public void startElement(String uri, String localName, String name, Attributes atts) {
                if ("rootfile".equals(localName) && atts.getValue("full-path") != null) {
                    rootFiles.add(atts.getValue("full-path"));
                }
            }
        }, context);
        if (rootFiles.isEmpty()) {
            throw new TikaException("The HWPX container names no package file");
        }
        return rootFiles.get(0);
    }

    /** Parses one XML part of the archive, at a path from the archive's root. */
    private static void parsePart(ZipFile zip, String path, DefaultHandler part,
                                  ParseContext context)
            throws IOException, SAXException, TikaException {
        final ZipEntry entry = zip.getEntry(path);
        if (entry == null) {
            throw new TikaException("The HWPX archive has no part " + path);
        }
        try (InputStream xml = zip.getInputStream(entry)) {
            XMLReaderUtils.parseSAX(xml, part, context);
        }
    }

    /** The package file: its metadata, the parts it lists and their reading order. */
    private static final class Package extends DefaultHandler {
        private final Metadata metadata;
        private final Map<String, String> manifest = new HashMap<>();
        private final List<String> spine = new ArrayList<>();
        private final StringBuilder text = new StringBuilder();
        private String collecting;

        Package(Metadata metadata) {
            this.metadata = metadata;
        }

        @Override
        public void startElement(String uri, String localName, String name, Attributes atts) {
            switch (localName) {
                case "item" -> manifest.put(atts.getValue("id"), atts.getValue("href"));
                case "itemref" -> spine.add(atts.getValue("idref"));
                case "title", "language" -> collecting = localName;
                case "meta" -> collecting = atts.getValue("name");
                default -> {
                    return;
                }
            }
            text.setLength(0);
        }

        @Override
        public void characters(char[] ch, int start, int length) {
            if (collecting != null) {
                text.append(ch, start, length);
            }
        }

        @Override
        public void endElement(String uri, String localName, String name) {
            if (collecting == null) {
                return;
            }
            final String value = text.toString().strip();
            if (!value.isEmpty()) {
                switch (collecting) {
                    case "title" -> metadata.set(TikaCoreProperties.TITLE, value);
                    case "language" -> metadata.set(TikaCoreProperties.LANGUAGE, value);
                    case "creator" -> metadata.add(TikaCoreProperties.CREATOR, value);
                    case "CreatedDate" -> metadata.set(TikaCoreProperties.CREATED, value);
                    case "ModifiedDate" -> metadata.set(TikaCoreProperties.MODIFIED, value);
                    default -> {
                    }
                }
            }
            collecting = null;
        }
    }

    /**
     * A section: paragraphs, and tables and notes, which HWPX nests inside the paragraph they are
     * anchored in. XHTML cannot nest paragraphs, so the text of a paragraph before a table or note
     * ends a paragraph of its own, and its text after it starts another. The paragraphs of a table
     * cell are its text, a line each, so a row stays a line of cells.
     */
    private static final class Section extends DefaultHandler {
        private final XHTMLContentHandler xhtml;
        private final Deque<StringBuilder> paragraphs = new ArrayDeque<>();
        private final Deque<Cell> cells = new ArrayDeque<>();
        private int textDepth;
        private int furnitureDepth;
        private boolean inSection;

        Section(XHTMLContentHandler xhtml) {
            this.xhtml = xhtml;
        }

        @Override
        public void startElement(String uri, String localName, String name, Attributes atts)
                throws SAXException {
            if ("sec".equals(localName)) {
                inSection = true;
            }
            if (!inSection) {
                return;
            }
            if (furnitureDepth > 0 || FURNITURE.contains(localName)) {
                furnitureDepth++;
                return;
            }
            switch (localName) {
                case "p" -> paragraphs.push(new StringBuilder());
                case "t" -> textDepth++;
                case "tab" -> append("\t");
                case "lineBreak" -> append("\n");
                case "tbl" -> {
                    endParagraph();
                    xhtml.startElement("table");
                }
                case "tr" -> xhtml.startElement("tr");
                case "tc" -> {
                    xhtml.startElement("td");
                    cells.push(new Cell(paragraphs.size()));
                }
                default -> {
                    if (NOTES.contains(localName)) {
                        endParagraph();
                        cells.push(new Cell(-1));
                    }
                }
            }
        }

        @Override
        public void characters(char[] ch, int start, int length) {
            if (textDepth > 0 && furnitureDepth == 0 && !paragraphs.isEmpty()) {
                paragraphs.peek().append(ch, start, length);
            }
        }

        @Override
        public void endElement(String uri, String localName, String name) throws SAXException {
            if (!inSection) {
                return;
            }
            if (furnitureDepth > 0) {
                furnitureDepth--;
                return;
            }
            switch (localName) {
                case "p" -> {
                    endParagraph();
                    paragraphs.pop();
                }
                case "t" -> textDepth--;
                case "tbl" -> xhtml.endElement("table");
                case "tr" -> xhtml.endElement("tr");
                case "tc" -> {
                    cells.pop();
                    xhtml.endElement("td");
                }
                case "sec" -> inSection = false;
                default -> {
                    if (NOTES.contains(localName)) {
                        cells.pop();
                    }
                }
            }
        }

        private void append(String text) {
            if (textDepth > 0 && !paragraphs.isEmpty()) {
                paragraphs.peek().append(text);
            }
        }

        /**
         * Emits the text the innermost open paragraph has collected so far: as a paragraph, or
         * as a line of the table cell it is in.
         */
        private void endParagraph() throws SAXException {
            final StringBuilder paragraph = paragraphs.peek();
            if (paragraph == null) {
                return;
            }
            final String text = paragraph.toString();
            paragraph.setLength(0);
            if (text.isBlank()) {
                return;
            }
            final Cell cell = cells.peek();
            if (cell == null || !cell.holds(paragraphs.size())) {
                xhtml.element("p", text);
                return;
            }
            if (cell.lines++ > 0) {
                xhtml.characters("\n");
            }
            xhtml.characters(text);
        }
    }

    /**
     * A table cell, or a note inside one, which takes paragraphs again: its paragraphs are those
     * one level deeper than the open ones it started inside, `depth`, or none for a note.
     */
    private static final class Cell {
        private final int depth;
        private int lines;

        Cell(int depth) {
            this.depth = depth;
        }

        boolean holds(int paragraphDepth) {
            return depth >= 0 && paragraphDepth == depth + 1;
        }
    }
}
