package ai.yobix;

import org.apache.commons.compress.compressors.snappy.SnappyCompressorInputStream;
import org.apache.tika.exception.TikaException;
import org.apache.tika.extractor.EmbeddedDocumentExtractor;
import org.apache.tika.extractor.EmbeddedDocumentUtil;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.metadata.TikaCoreProperties;
import org.apache.tika.mime.MediaType;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.apache.tika.sax.XHTMLContentHandler;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.regex.Matcher;
import java.util.regex.Pattern;
import java.util.zip.ZipEntry;
import java.util.zip.ZipFile;
import java.util.zip.ZipInputStream;

/**
 * Pages, Numbers and Keynote documents from iWork 2013 on, whose content is stored as
 * Snappy-compressed protobuf archives (.iwa). Tika only lists the files of such a package; this
 * reads the text of its documents, slides and text boxes, and the strings of its tables, and hands
 * the files in its Data folder to the embedded document extractor as Tika does.
 */
final class IworkArchiveParser implements Parser {

    private static final Set<MediaType> TYPES = Set.of(
            MediaType.application("vnd.apple.pages.13"),
            MediaType.application("vnd.apple.numbers.13"),
            MediaType.application("vnd.apple.keynote.13"),
            MediaType.application("vnd.apple.unknown.13"),
            MediaType.application("vnd.apple.pages.18"),
            MediaType.application("vnd.apple.numbers.18"),
            MediaType.application("vnd.apple.keynote.18"));

    /** TSWP.StorageArchive: a text storage, its text in field 3. */
    private static final int STORAGE = 2001;
    /** TST.TableDataList: a table's list of values, its type in field 1, entries in field 3. */
    private static final int TABLE_DATA_LIST = 6005;
    private static final int STRING_LIST = 1;
    private static final long MAX_DECOMPRESSED_BYTES = 256L << 20;
    private static final int CHUNK_HEADER_BYTES = 4;
    private static final String ATTACHMENT = "￼";
    private static final Pattern NUMBERED = Pattern.compile("(.*?)-(\\d+)\\.iwa$");

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return TYPES;
    }

    @Override
    public void parse(InputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        final TikaInputStream input = TikaInputStream.get(stream);
        final XHTMLContentHandler xhtml = new XHTMLContentHandler(handler, metadata);
        xhtml.startDocument();
        try (ZipFile zip = new ZipFile(input.getFile())) {
            final Budget budget = new Budget();
            final Map<String, byte[]> archives = archives(zip, budget);
            final List<String> names = new ArrayList<>(archives.keySet());
            names.sort(Comparator.comparing(name -> readingOrder(fileName(name))));
            for (final String name : names) {
                if (isTemplate(fileName(name))) {
                    continue;
                }
                for (final String paragraph : text(decompress(archives.get(name), budget))) {
                    xhtml.element("p", paragraph);
                }
            }
            embedded(zip, xhtml, context);
        }
        xhtml.endDocument();
    }

    /** Caps how much an archive may decompress to, against decompression bombs. */
    private static final class Budget {
        private long remaining = MAX_DECOMPRESSED_BYTES;

        void spend(long bytes) throws TikaException {
            remaining -= bytes;
            if (remaining < 0) {
                throw new TikaException("The iWork archives decompress to more than "
                        + MAX_DECOMPRESSED_BYTES + " bytes");
            }
        }
    }

    /** The compressed .iwa archives of the package, whether stored directly or in Index.zip. */
    private static Map<String, byte[]> archives(ZipFile zip, Budget budget)
            throws IOException, TikaException {
        final Map<String, byte[]> archives = new TreeMap<>();
        final var entries = zip.entries();
        while (entries.hasMoreElements()) {
            final ZipEntry entry = entries.nextElement();
            if (entry.getName().endsWith(".iwa")) {
                try (InputStream archive = zip.getInputStream(entry)) {
                    archives.put(entry.getName(), read(archive, budget));
                }
            } else if (entry.getName().endsWith("Index.zip")) {
                try (ZipInputStream index = new ZipInputStream(zip.getInputStream(entry))) {
                    ZipEntry inner;
                    while ((inner = index.getNextEntry()) != null) {
                        if (inner.getName().endsWith(".iwa")) {
                            archives.put(entry.getName() + "/" + inner.getName(),
                                    read(index, budget));
                        }
                    }
                }
            }
        }
        return archives;
    }

    private static String fileName(String path) {
        return path.substring(path.lastIndexOf('/') + 1);
    }

    /** Master slides hold the placeholders of a theme, not the presentation's text. */
    private static boolean isTemplate(String name) {
        return name.startsWith("MasterSlide");
    }

    /**
     * The document first, then the slides, then everything else (such as tables' lists), each
     * group in the order of the archives' numbers.
     */
    private static String readingOrder(String name) {
        if (name.equals("Document.iwa")) {
            return "0";
        }
        final String group = name.startsWith("Slide") ? "1" : "2";
        final Matcher numbered = NUMBERED.matcher(name);
        if (numbered.matches()) {
            return group + numbered.group(1)
                    + String.format("%010d", Long.parseLong(numbered.group(2)));
        }
        return group + name.replace(".iwa", "");
    }

    private static byte[] read(InputStream input, Budget budget) throws IOException, TikaException {
        final ByteArrayOutputStream bytes = new ByteArrayOutputStream();
        final byte[] buffer = new byte[8192];
        int read;
        while ((read = input.read(buffer)) != -1) {
            budget.spend(read);
            bytes.write(buffer, 0, read);
        }
        return bytes.toByteArray();
    }

    /**
     * An .iwa file's protobuf stream: chunks of a zero byte, a three-byte little-endian length
     * and a raw Snappy block.
     */
    private static byte[] decompress(byte[] archive, Budget budget) throws IOException, TikaException {
        final ByteArrayOutputStream stream = new ByteArrayOutputStream();
        int offset = 0;
        while (offset < archive.length) {
            if (archive[offset] != 0 || offset + CHUNK_HEADER_BYTES > archive.length) {
                throw new TikaException("Corrupt iWork archive chunk at byte " + offset);
            }
            final int length = (archive[offset + 1] & 0xff) | (archive[offset + 2] & 0xff) << 8
                    | (archive[offset + 3] & 0xff) << 16;
            offset += CHUNK_HEADER_BYTES;
            if (offset + length > archive.length) {
                throw new TikaException("Truncated iWork archive chunk at byte " + offset);
            }
            try (InputStream block = new SnappyCompressorInputStream(
                    new ByteArrayInputStream(archive, offset, length))) {
                stream.write(read(block, budget));
            }
            offset += length;
        }
        return stream.toByteArray();
    }

    /** The paragraphs of the text storages and string tables in an archive's messages. */
    private static List<String> text(byte[] stream) throws TikaException {
        final List<String> paragraphs = new ArrayList<>();
        final Protobuf reader = new Protobuf(stream, 0, stream.length);
        while (reader.hasMore()) {
            final Protobuf info = reader.message(reader.varint());
            final List<long[]> messages = new ArrayList<>();
            while (info.hasMore()) {
                final int field = info.field();
                if (field == 2) {
                    final Protobuf messageInfo = info.message(info.varint());
                    long type = -1;
                    long length = 0;
                    while (messageInfo.hasMore()) {
                        switch (messageInfo.field()) {
                            case 1 -> type = messageInfo.varint();
                            case 3 -> length = messageInfo.varint();
                            default -> messageInfo.skip();
                        }
                    }
                    messages.add(new long[] {type, length});
                } else {
                    info.skip();
                }
            }
            for (final long[] message : messages) {
                final Protobuf payload = reader.message(message[1]);
                if (message[0] == STORAGE) {
                    storage(payload, paragraphs);
                } else if (message[0] == TABLE_DATA_LIST) {
                    strings(payload, paragraphs);
                }
            }
        }
        return paragraphs;
    }

    private static void storage(Protobuf storage, List<String> paragraphs) throws TikaException {
        while (storage.hasMore()) {
            if (storage.field() == 3) {
                for (final String line : storage.string().replace(ATTACHMENT, "")
                        .split("[\n  ]")) {
                    if (!line.isBlank()) {
                        paragraphs.add(line.strip());
                    }
                }
            } else {
                storage.skip();
            }
        }
    }

    /** The strings of a table's string list, in the order of their keys. */
    private static void strings(Protobuf list, List<String> paragraphs) throws TikaException {
        long listType = -1;
        final Map<Long, String> strings = new TreeMap<>();
        while (list.hasMore()) {
            switch (list.field()) {
                case 1 -> listType = list.varint();
                case 3 -> {
                    final Protobuf entry = list.message(list.varint());
                    long key = -1;
                    String string = null;
                    while (entry.hasMore()) {
                        switch (entry.field()) {
                            case 1 -> key = entry.varint();
                            case 3 -> string = entry.string();
                            default -> entry.skip();
                        }
                    }
                    if (string != null && !string.isBlank()) {
                        strings.put(key, string.strip());
                    }
                }
                default -> list.skip();
            }
        }
        if (listType == STRING_LIST) {
            paragraphs.addAll(strings.values());
        }
    }

    /** Hands the files of the package's Data folder, its images and media, to the extractor. */
    private static void embedded(ZipFile zip, XHTMLContentHandler xhtml, ParseContext context)
            throws IOException, SAXException {
        final EmbeddedDocumentExtractor extractor =
                EmbeddedDocumentUtil.getEmbeddedDocumentExtractor(context);
        final var entries = zip.entries();
        while (entries.hasMoreElements()) {
            final ZipEntry entry = entries.nextElement();
            if (entry.isDirectory() || !entry.getName().contains("Data/")) {
                continue;
            }
            final Metadata embedded = new Metadata();
            embedded.set(TikaCoreProperties.RESOURCE_NAME_KEY, fileName(entry.getName()));
            embedded.set(TikaCoreProperties.EMBEDDED_RELATIONSHIP_ID, entry.getName());
            if (!extractor.shouldParseEmbedded(embedded)) {
                continue;
            }
            try (TikaInputStream file = TikaInputStream.get(zip.getInputStream(entry))) {
                extractor.parseEmbedded(file, xhtml, embedded, true);
            }
        }
    }

    /** Reads protobuf wire format: varints, length-delimited fields, and skips the rest. */
    private static final class Protobuf {
        private final byte[] bytes;
        private int offset;
        private final int end;
        private int wireType;

        Protobuf(byte[] bytes, int offset, int end) {
            this.bytes = bytes;
            this.offset = offset;
            this.end = end;
        }

        boolean hasMore() {
            return offset < end;
        }

        int field() throws TikaException {
            final long key = varint();
            wireType = (int) (key & 7);
            return (int) (key >>> 3);
        }

        long varint() throws TikaException {
            long value = 0;
            for (int shift = 0; shift < 64; shift += 7) {
                if (offset >= end) {
                    throw new TikaException("Truncated varint in an iWork archive");
                }
                final byte next = bytes[offset++];
                value |= (long) (next & 0x7f) << shift;
                if (next >= 0) {
                    return value;
                }
            }
            throw new TikaException("Overlong varint in an iWork archive");
        }

        Protobuf message(long length) throws TikaException {
            if (length < 0 || length > end - offset) {
                throw new TikaException("A message runs past the end of an iWork archive");
            }
            final Protobuf message = new Protobuf(bytes, offset, offset + (int) length);
            offset += (int) length;
            return message;
        }

        String string() throws TikaException {
            final long length = varint();
            if (length < 0 || length > end - offset) {
                throw new TikaException("A string runs past the end of an iWork archive");
            }
            final String value = new String(bytes, offset, (int) length, StandardCharsets.UTF_8);
            offset += (int) length;
            return value;
        }

        void skip() throws TikaException {
            switch (wireType) {
                case 0 -> varint();
                case 1 -> offset += 8;
                case 2 -> message(varint());
                case 5 -> offset += 4;
                default -> throw new TikaException(
                        "Unsupported protobuf wire type " + wireType + " in an iWork archive");
            }
            if (offset > end) {
                throw new TikaException("A field runs past the end of an iWork archive");
            }
        }
    }
}
