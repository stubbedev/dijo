package ai.yobix;

import org.apache.tika.exception.EncryptedDocumentException;
import org.apache.tika.exception.TikaException;
import org.apache.tika.exception.UnsupportedFormatException;
import org.apache.tika.io.TikaInputStream;
import org.apache.tika.metadata.HttpHeaders;
import org.apache.tika.metadata.Metadata;
import org.apache.tika.metadata.TikaCoreProperties;
import org.apache.tika.mime.MediaType;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.Parser;
import org.xml.sax.ContentHandler;
import org.xml.sax.SAXException;

import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.ByteBuffer;
import java.nio.charset.Charset;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.Arrays;
import java.util.Map;
import java.util.Set;

/**
 * Mobipocket and Kindle books (MOBI, AZW, AZW3) without DRM: the text records of the book,
 * decompressed and read as the HTML they hold, with the title, authors and language of the
 * book's header as metadata. Tika detects these books but has no parser for them.
 */
final class MobiParser implements Parser {

    static final MediaType MOBI = MediaType.application("x-mobipocket-ebook");

    private static final int PALM_DATABASE_HEADER = 78;
    private static final int RECORD_ENTRY = 8;
    private static final int NO_COMPRESSION = 1;
    private static final int PALMDOC_COMPRESSION = 2;
    private static final int HUFF_CDIC_COMPRESSION = 17480;
    private static final int MOBI_HEADER = 16;
    private static final int UTF_8 = 65001;
    private static final int EXTRA_FLAGS_MIN_HEADER_LENGTH = 0xE4;
    private static final int EXTRA_FLAGS = 0xF2;
    private static final int EXTH_PRESENT = 0x40;
    private static final long MAX_TEXT_BYTES = 256L << 20;
    private static final Map<Integer, String> EXTH_AUTHOR_TITLE_LANGUAGE =
            Map.of(100, "author", 503, "title", 524, "language");

    private final Parser html;

    /** @param html parses the HTML the book's text records hold */
    MobiParser(Parser html) {
        this.html = html;
    }

    @Override
    public Set<MediaType> getSupportedTypes(ParseContext context) {
        return Set.of(MOBI);
    }

    @Override
    public void parse(TikaInputStream stream, ContentHandler handler, Metadata metadata,
                      ParseContext context) throws IOException, SAXException, TikaException {
        final ByteBuffer book = ByteBuffer.wrap(Files.readAllBytes(TikaInputStream.get(stream).getPath()));
        if (book.limit() < PALM_DATABASE_HEADER) {
            throw new TikaException("The book is shorter than a Palm database header");
        }
        final int records = book.getShort(76) & 0xffff;
        final int[] offsets = new int[records + 1];
        for (int index = 0; index < records; index++) {
            offsets[index] = book.getInt(PALM_DATABASE_HEADER + index * RECORD_ENTRY);
        }
        offsets[records] = book.limit();
        for (int index = 0; index < records; index++) {
            if (offsets[index] < 0 || offsets[index] > offsets[index + 1]) {
                throw new TikaException("The book's record " + index + " lies outside the file");
            }
        }
        final int header = offsets[0];
        final int compression = book.getShort(header) & 0xffff;
        final long textLength = book.getInt(header + 4) & 0xffffffffL;
        final int textRecords = book.getShort(header + 8) & 0xffff;
        final int encryption = book.getShort(header + 12) & 0xffff;
        if (encryption != 0) {
            throw new EncryptedDocumentException("The book is protected by DRM");
        }
        if (compression == HUFF_CDIC_COMPRESSION) {
            throw new UnsupportedFormatException("HUFF/CDIC compressed books are not supported");
        }
        if (compression != NO_COMPRESSION && compression != PALMDOC_COMPRESSION) {
            throw new TikaException("Unknown book compression " + compression);
        }
        if (textRecords >= records || textLength > MAX_TEXT_BYTES) {
            throw new TikaException("The book's header names more text than the book holds");
        }
        final boolean mobi = header + MOBI_HEADER + 4 <= book.limit()
                && book.getInt(header + MOBI_HEADER) == 0x4D4F4249;
        final int mobiLength = mobi ? book.getInt(header + MOBI_HEADER + 4) : 0;
        final int extraFlags = mobi && mobiLength >= EXTRA_FLAGS_MIN_HEADER_LENGTH
                ? book.getShort(header + EXTRA_FLAGS) & 0xffff : 0;
        final Charset charset = mobi && book.getInt(header + MOBI_HEADER + 12) == UTF_8
                ? StandardCharsets.UTF_8 : Charset.forName("windows-1252");

        final ByteArrayOutputStream text = new ByteArrayOutputStream((int) textLength);
        for (int index = 1; index <= textRecords; index++) {
            final byte[] record = new byte[offsets[index + 1] - offsets[index]];
            book.get(offsets[index], record);
            final int content = record.length - trailingBytes(record, extraFlags);
            if (compression == PALMDOC_COMPRESSION) {
                text.write(decompress(record, content));
            } else {
                text.write(record, 0, content);
            }
            if (text.size() > MAX_TEXT_BYTES) {
                throw new TikaException("The book's text decompresses past " + MAX_TEXT_BYTES + " bytes");
            }
        }

        final Metadata markup = new Metadata();
        markup.set(HttpHeaders.CONTENT_TYPE, "text/html; charset=" + charset.name());
        try (TikaInputStream markupStream = TikaInputStream.get(text.toByteArray())) {
            html.parse(markupStream, handler, markup, context);
        }
        if (mobi) {
            bookMetadata(book, header, mobiLength, charset, metadata);
        }
    }

    /**
     * The size of the entries the book's extra data flags say end a text record: one entry,
     * sized by a backward variable-length integer, per flag from the second on, then the bytes of
     * a multibyte character cut by the record's end when the first flag is set.
     */
    static int trailingBytes(byte[] record, int extraFlags) throws TikaException {
        int trailing = 0;
        for (int flags = extraFlags >>> 1; flags != 0; flags >>>= 1) {
            if ((flags & 1) != 0) {
                trailing += entrySize(record, record.length - trailing);
            }
        }
        if ((extraFlags & 1) != 0) {
            final int last = record.length - trailing - 1;
            if (last < 0) {
                throw new TikaException("A text record ends before its trailing entries");
            }
            trailing += (record[last] & 0x3) + 1;
        }
        if (trailing > record.length) {
            throw new TikaException("A text record ends before its trailing entries");
        }
        return trailing;
    }

    private static int entrySize(byte[] record, int end) {
        int size = 0;
        for (int index = Math.max(0, end - 4); index < end; index++) {
            if ((record[index] & 0x80) != 0) {
                size = 0;
            }
            size = (size << 7) | (record[index] & 0x7f);
        }
        return size;
    }

    /** PalmDOC's LZ77: literals, runs of literals, space-prefixed characters and back copies. */
    static byte[] decompress(byte[] record, int length) throws TikaException {
        byte[] out = new byte[Math.max(16, length * 2)];
        int size = 0;
        int index = 0;
        while (index < length) {
            final int code = record[index++] & 0xff;
            if (code == 0 || (code >= 0x09 && code <= 0x7f)) {
                out = room(out, size + 1);
                out[size++] = (byte) code;
            } else if (code <= 0x08) {
                if (index + code > length) {
                    throw new TikaException("A literal run passes the end of a text record");
                }
                out = room(out, size + code);
                System.arraycopy(record, index, out, size, code);
                size += code;
                index += code;
            } else if (code >= 0xc0) {
                out = room(out, size + 2);
                out[size++] = ' ';
                out[size++] = (byte) (code ^ 0x80);
            } else {
                if (index >= length) {
                    throw new TikaException("A back copy passes the end of a text record");
                }
                final int pair = code << 8 | (record[index++] & 0xff);
                final int distance = (pair >> 3) & 0x7ff;
                final int count = (pair & 0x7) + 3;
                if (distance == 0 || distance > size) {
                    throw new TikaException("A back copy reaches before the start of a text record");
                }
                out = room(out, size + count);
                for (int copied = 0; copied < count; copied++) {
                    out[size] = out[size - distance];
                    size++;
                }
            }
        }
        return Arrays.copyOf(out, size);
    }

    private static byte[] room(byte[] buffer, int needed) {
        return needed <= buffer.length ? buffer : Arrays.copyOf(buffer, Math.max(needed, buffer.length * 2));
    }

    /** The title, authors and language of the book's MOBI and EXTH headers. */
    private static void bookMetadata(ByteBuffer book, int header, int mobiLength, Charset charset,
                                     Metadata metadata) {
        final int nameOffset = book.getInt(header + 0x54);
        final int nameLength = book.getInt(header + 0x58);
        if (nameOffset > 0 && nameLength > 0 && header + nameOffset + nameLength <= book.limit()) {
            final byte[] name = new byte[nameLength];
            book.get(header + nameOffset, name);
            metadata.set(TikaCoreProperties.TITLE, new String(name, charset));
        }
        final int flags = book.getInt(header + MOBI_HEADER + 0x70);
        final int exth = header + MOBI_HEADER + mobiLength;
        if ((flags & EXTH_PRESENT) == 0 || exth + 12 > book.limit()
                || book.getInt(exth) != 0x45585448) {
            return;
        }
        final int count = book.getInt(exth + 8);
        int offset = exth + 12;
        for (int entry = 0; entry < count && offset + 8 <= book.limit(); entry++) {
            final int type = book.getInt(offset);
            final int length = book.getInt(offset + 4);
            if (length < 8 || offset + length > book.limit()) {
                return;
            }
            final String field = EXTH_AUTHOR_TITLE_LANGUAGE.get(type);
            if (field != null) {
                final byte[] value = new byte[length - 8];
                book.get(offset + 8, value);
                final String text = new String(value, charset).strip();
                switch (field) {
                    case "author" -> metadata.add(TikaCoreProperties.CREATOR, text);
                    case "title" -> metadata.set(TikaCoreProperties.TITLE, text);
                    default -> metadata.set(TikaCoreProperties.LANGUAGE, text);
                }
            }
            offset += length;
        }
    }
}
