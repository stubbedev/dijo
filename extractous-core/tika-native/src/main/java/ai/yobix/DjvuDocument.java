package ai.yobix;

import org.apache.tika.exception.TikaException;
import org.apache.tika.exception.UnsupportedFormatException;

import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.channels.FileChannel;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/**
 * The structure of a DjVu file (DjVu v3 specification, section 8): a single page FORM:DJVU, or a
 * bundled FORM:DJVM whose DIRM chunk names the component files that follow it, pages and the
 * FORM:DJVI files they share by INCL. Chunks are read from the file as they are needed.
 */
final class DjvuDocument {

    static final int MAX_CHUNK_BYTES = 256 << 20;

    private static final int CHUNK_HEADER = 8;
    private static final int MAGIC = 0x41542654;
    private static final int BUNDLED = 0x80;
    private static final int MAX_DIRECTORY_BYTES = 16 << 20;

    /** An IFF chunk: its id, where its data starts, its size, its end and the next chunk. */
    record Chunk(String id, long data, long size, long end, long next) {
    }

    /** A FORM chunk: its type, and where its nested chunks start and end. */
    record Form(String type, long start, long end) {
    }

    private final FileChannel file;
    private final List<Form> pages;
    private final Map<String, Form> components;

    private DjvuDocument(FileChannel file, List<Form> pages, Map<String, Form> components) {
        this.file = file;
        this.pages = pages;
        this.components = components;
    }

    /** The document an open file holds; the file must stay open while the document is used. */
    static DjvuDocument read(FileChannel file) throws IOException, TikaException {
        if (file.size() < 4 + CHUNK_HEADER + 4 || readInt(file, 0) != MAGIC) {
            throw new TikaException("Not a DjVu file: no AT&T magic");
        }
        final Form root = form(file, chunk(file, 4, file.size()));
        if ("DJVU".equals(root.type)) {
            return new DjvuDocument(file, List.of(root), Map.of());
        }
        if (!"DJVM".equals(root.type)) {
            throw new TikaException("Unknown DjVu FORM type " + root.type);
        }
        final List<Form> pages = new ArrayList<>();
        final List<Form> files = new ArrayList<>();
        List<String> ids = null;
        for (long offset = root.start; offset < root.end; ) {
            final Chunk chunk = chunk(file, offset, root.end);
            if ("DIRM".equals(chunk.id)) {
                ids = directory(file, chunk);
            } else if ("FORM".equals(chunk.id)) {
                final Form form = form(file, chunk);
                files.add(form);
                if ("DJVU".equals(form.type)) {
                    pages.add(form);
                }
            }
            offset = chunk.next;
        }
        if (ids == null) {
            throw new TikaException("A multi-page DjVu document has no DIRM directory");
        }
        if (ids.size() != files.size()) {
            throw new TikaException("The DjVu directory names " + ids.size()
                    + " component files, but the document bundles " + files.size());
        }
        final Map<String, Form> components = new HashMap<>();
        for (int i = 0; i < ids.size(); i++) {
            components.put(ids.get(i), files.get(i));
        }
        return new DjvuDocument(file, List.copyOf(pages), Map.copyOf(components));
    }

    List<Form> pages() {
        return pages;
    }

    /** The component file an INCL chunk names. */
    Form component(String id) throws TikaException {
        final Form form = components.get(id);
        if (form == null) {
            throw new TikaException("The DjVu document has no component file " + id);
        }
        return form;
    }

    /** The chunks a FORM holds, in order. */
    List<Chunk> chunks(Form form) throws IOException, TikaException {
        final List<Chunk> chunks = new ArrayList<>();
        for (long offset = form.start; offset < form.end; ) {
            final Chunk chunk = chunk(file, offset, form.end);
            chunks.add(chunk);
            offset = chunk.next;
        }
        return chunks;
    }

    /** A chunk's data. */
    byte[] data(Chunk chunk) throws IOException, TikaException {
        if (chunk.size > MAX_CHUNK_BYTES) {
            throw new TikaException("The DjVu chunk " + chunk.id + " of " + chunk.size
                    + " bytes exceeds " + MAX_CHUNK_BYTES);
        }
        return read(file, chunk.data, (int) chunk.size);
    }

    /**
     * The component ids of a bundled document's directory, in the order of its component files
     * (8.3.2): after the flags, the file count and the offsets, a BZZ stream of the sizes, the
     * flags and the id strings, each followed by a name and a title when its flags say so.
     */
    private static List<String> directory(FileChannel file, Chunk chunk)
            throws IOException, TikaException {
        if (chunk.size < 3) {
            throw new TikaException("The DjVu directory ends inside its header");
        }
        if ((readByte(file, chunk.data) & BUNDLED) == 0) {
            throw new UnsupportedFormatException(
                    "An indirect DjVu document keeps its pages in other files");
        }
        final int count = ((readByte(file, chunk.data + 1) << 8) | readByte(file, chunk.data + 2));
        final long compressed = chunk.data + 3 + 4L * count;
        if (compressed > chunk.end) {
            throw new TikaException("The DjVu directory ends inside its offsets");
        }
        final byte[] listing = BzzDecoder.decode(
                read(file, compressed, (int) (chunk.end - compressed)), MAX_DIRECTORY_BYTES);
        int at = 4 * count;
        if (at > listing.length) {
            throw new TikaException("The DjVu directory ends inside its file sizes and flags");
        }
        final List<String> ids = new ArrayList<>(count);
        for (int i = 0; i < count; i++) {
            final int flags = listing[3 * count + i] & 0xff;
            final int strings = 1 + ((flags & 0x80) != 0 ? 1 : 0) + ((flags & 0x40) != 0 ? 1 : 0);
            for (int s = 0; s < strings; s++) {
                int end = at;
                while (end < listing.length && listing[end] != 0) {
                    end++;
                }
                if (end == listing.length) {
                    throw new TikaException("The DjVu directory ends inside a file name");
                }
                if (s == 0) {
                    ids.add(new String(listing, at, end - at, StandardCharsets.UTF_8));
                }
                at = end + 1;
            }
        }
        return ids;
    }

    private static Form form(FileChannel file, Chunk chunk) throws IOException, TikaException {
        if (!"FORM".equals(chunk.id) || chunk.size < 4) {
            throw new TikaException("Expected a DjVu FORM chunk, not " + chunk.id);
        }
        return new Form(readId(file, chunk.data), chunk.data + 4, chunk.end);
    }

    private static Chunk chunk(FileChannel file, long offset, long limit)
            throws IOException, TikaException {
        if (offset + CHUNK_HEADER > limit) {
            throw new TikaException("A DjVu chunk header at " + offset + " passes its container");
        }
        final String id = readId(file, offset);
        final long size = readInt(file, offset + 4) & 0xffffffffL;
        final long data = offset + CHUNK_HEADER;
        if (data + size > limit) {
            throw new TikaException("The DjVu chunk " + id + " at " + offset
                    + " passes its container");
        }
        final long end = data + size;
        return new Chunk(id, data, size, end, Math.min(limit, end + (end & 1)));
    }

    private static String readId(FileChannel file, long offset) throws IOException, TikaException {
        return new String(read(file, offset, 4), StandardCharsets.US_ASCII);
    }

    private static int readInt(FileChannel file, long offset) throws IOException, TikaException {
        return ByteBuffer.wrap(read(file, offset, 4)).getInt();
    }

    private static int readByte(FileChannel file, long offset) throws IOException, TikaException {
        return read(file, offset, 1)[0] & 0xff;
    }

    private static byte[] read(FileChannel file, long offset, int length)
            throws IOException, TikaException {
        final ByteBuffer buffer = ByteBuffer.allocate(length);
        while (buffer.hasRemaining()) {
            if (file.read(buffer, offset + buffer.position()) < 0) {
                throw new TikaException("The DjVu file ends inside a chunk at " + offset);
            }
        }
        return buffer.array();
    }
}
