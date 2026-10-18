import java.io.ByteArrayOutputStream;
import java.io.DataOutputStream;
import java.io.IOException;
import java.nio.ByteBuffer;
import java.nio.CharBuffer;
import java.nio.charset.Charset;
import java.nio.charset.CharsetDecoder;
import java.nio.charset.CoderResult;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.TreeSet;
import java.util.zip.Deflater;
import java.util.zip.DeflaterOutputStream;

/**
 * Writes the JDK's charsets as the Rust port of Tika's encoding detection needs them: each
 * charset's canonical name and aliases, and the tables to decode it exactly as the JDK does.
 *
 * A single-byte charset has the character each byte decodes to (U+FFFD where it decodes to
 * none). A charset whose decoder is one of the JDK's East Asian multi-byte decoders has what its
 * strict decoder makes of each byte, of each pair of bytes starting with a byte that needs more,
 * and of the longer sequences of the families that have them, as cells: a code point, a malformed
 * or unmappable input of a length ({@link #ERROR} plus the length), input that needs more bytes
 * ({@link #MORE}) or a cell no decoding reads ({@link #UNUSED}). The decoder loops around the
 * tables are ported from the JDK's sources.
 *
 * Format, big-endian: "JCS2", the charset count (u16), then per charset its name, its alias count
 * (u16) and aliases (each a u16 length and UTF-8), then its kind (u8) and data: none for 0, the
 * 256 u16 characters of a single-byte charset for 1, and for the multi-byte families 2 to 6 the
 * length (u32) of a zlib stream of a table count (u8) and the tables, each a cell count (u32) and
 * its u32 cells, each less the one before it (wrapping), which compresses the runs of
 * consecutive characters.
 *
 * The decoder reference has one line per multi-byte charset and per charset of
 * {@link #PORTED}: its name and, in hexadecimal and separated by a tab, the hash
 * {@link DecoderReference} takes of what its decoder makes of pseudo-random bytes (of the
 * decoded text alone for a charset of {@link #PORTED}), and one per charset of {@link #ISO_2022}
 * with the hash of what its decoder makes of pseudo-random escapes, shifts and bytes.
 */
public final class CharsetTables {

    static final int ERROR = 0x8000_0000;
    static final int MORE = 0xC000_0000;
    static final int UNUSED = 0xFFFF_FFFF;

    static final int NONE = 0;
    static final int SINGLE_BYTE = 1;
    static final int DOUBLE_BYTE = 2;
    static final int HKSCS = 3;
    static final int EUC_JP = 4;
    static final int EUC_TW = 5;
    static final int GB18030 = 6;

    /** The charsets whose decoders the Rust port mirrors without tables. */
    static final List<String> PORTED =
            List.of("UTF-8", "UTF-16", "UTF-16BE", "UTF-16LE", "x-ISCII91");

    /** The ISO-2022 charsets whose decoders the Rust port mirrors around the multi-byte tables. */
    static final List<String> ISO_2022 =
            List.of("ISO-2022-KR", "ISO-2022-CN", "x-ISO-2022-CN-CNS", "x-ISO-2022-CN-GB");

    public static void main(String[] args) throws IOException {
        if (args.length != 2) {
            System.err.println("usage: CharsetTables <tables> <decoder reference>");
            System.exit(2);
        }
        final List<String> reference = new ArrayList<>();
        final ByteArrayOutputStream bytes = new ByteArrayOutputStream();
        final DataOutputStream out = new DataOutputStream(bytes);
        out.writeBytes("JCS2");
        final Map<String, Charset> charsets = Charset.availableCharsets();
        out.writeShort(charsets.size());
        for (Charset charset : charsets.values()) {
            writeString(out, charset.name());
            final TreeSet<String> aliases = new TreeSet<>(charset.aliases());
            out.writeShort(aliases.size());
            for (String alias : aliases) {
                writeString(out, alias);
            }
            if (writeDecoding(out, charset)) {
                reference.add(charset.name() + "\t" + Long.toHexString(DecoderReference.hash(charset)));
            }
        }
        for (String name : PORTED) {
            reference.add(name + "\t" + Long.toHexString(DecoderReference.textHash(
                    Charset.forName(name))));
        }
        for (String name : ISO_2022) {
            reference.add(name + "\t" + Long.toHexString(DecoderReference.iso2022Hash(
                    Charset.forName(name))));
        }
        out.flush();
        Files.write(Path.of(args[0]), bytes.toByteArray());
        Files.write(Path.of(args[1]), reference);
    }

    private static void writeString(DataOutputStream out, String text) throws IOException {
        final byte[] utf8 = text.getBytes(StandardCharsets.UTF_8);
        out.writeShort(utf8.length);
        out.write(utf8);
    }

    /** Writes the tables to decode a charset with, telling whether it is a multi-byte one. */
    private static boolean writeDecoding(DataOutputStream out, Charset charset) throws IOException {
        final char[] singleByte = singleByteTable(charset);
        if (singleByte != null) {
            out.writeByte(SINGLE_BYTE);
            for (char c : singleByte) {
                out.writeChar(c);
            }
            return false;
        }
        final int family = family(charset);
        if (family == NONE) {
            out.writeByte(NONE);
            return false;
        }
        final ByteArrayOutputStream compressed = new ByteArrayOutputStream();
        final Deflater deflater = new Deflater(Deflater.BEST_COMPRESSION);
        try (DataOutputStream data = new DataOutputStream(new DeflaterOutputStream(compressed, deflater))) {
            final List<int[]> tables = multiByteTables(charset, family);
            data.writeByte(tables.size());
            for (int[] table : tables) {
                data.writeInt(table.length);
                int previous = 0;
                for (int cell : table) {
                    data.writeInt(cell - previous);
                    previous = cell;
                }
            }
        }
        deflater.end();
        out.writeByte(family);
        out.writeInt(compressed.size());
        compressed.writeTo(out);
        return true;
    }

    /** What each byte decodes to alone, or null when the charset is not one byte per character. */
    private static char[] singleByteTable(Charset charset) {
        if (!charset.canEncode() || charset.newEncoder().maxBytesPerChar() != 1.0f
                || charset.newDecoder().maxCharsPerByte() != 1.0f
                || charset.name().toUpperCase(Locale.ROOT).contains("UTF")) {
            return null;
        }
        final char[] table = new char[256];
        final byte[] all = new byte[256];
        for (int b = 0; b < 256; b++) {
            all[b] = (byte) b;
            final CharsetDecoder decoder = charset.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPLACE)
                    .onUnmappableCharacter(CodingErrorAction.REPLACE);
            final CharBuffer decoded;
            try {
                decoded = decoder.decode(ByteBuffer.wrap(new byte[]{(byte) b}));
            } catch (IOException e) {
                return null;
            }
            if (decoded.length() != 1) {
                return null;
            }
            table[b] = decoded.charAt(0);
        }
        final String whole = new String(all, charset);
        if (whole.length() != 256 || !whole.equals(new String(table))) {
            return null;
        }
        return table;
    }

    /** The multi-byte decoder family whose loop the Rust port has, by the decoder's class. */
    private static int family(Charset charset) {
        if (!charset.canEncode()) {
            return NONE;
        }
        for (Class<?> c = charset.newDecoder().getClass(); c != null; c = c.getSuperclass()) {
            switch (c.getName()) {
                case "sun.nio.cs.DoubleByte$Decoder":
                    return c == charset.newDecoder().getClass() ? DOUBLE_BYTE : NONE;
                case "sun.nio.cs.HKSCS$Decoder":
                    return HKSCS;
                case "sun.nio.cs.EUC_JP$Decoder":
                    return EUC_JP;
                case "sun.nio.cs.EUC_TW$Decoder":
                    return EUC_TW;
                case "sun.nio.cs.GB18030$Decoder":
                    return GB18030;
                default:
            }
        }
        return NONE;
    }

    private static List<int[]> multiByteTables(Charset charset, int family) {
        final List<int[]> tables = new ArrayList<>();
        final int[] singles = new int[256];
        for (int b = 0; b < 256; b++) {
            singles[b] = cell(charset, b);
            if (b < 0x80 && singles[b] != b
                    && !new String(new byte[]{(byte) b}, charset).equals(Character.toString(singles[b]))) {
                throw new IllegalStateException(charset + " decodes ASCII " + b + " differently in a String");
            }
        }
        tables.add(singles);
        final int[] pairs = new int[0x80 * 256];
        for (int b0 = 0x80; b0 < 0x100; b0++) {
            for (int b1 = 0; b1 < 256; b1++) {
                pairs[(b0 - 0x80) * 256 + b1] = singles[b0] == MORE ? cell(charset, b0, b1) : UNUSED;
            }
        }
        tables.add(pairs);
        switch (family) {
            case EUC_JP -> tables.add(longer(charset, 0x8F));
            case EUC_TW -> {
                final int[] planes = new int[256];
                final List<int[]> planeTables = new ArrayList<>();
                for (int plane = 0; plane < 256; plane++) {
                    final int[] table = longer(charset, 0x8E, plane);
                    final boolean invalid = table[0] == (ERROR | 2);
                    for (int cell : table) {
                        if ((cell == (ERROR | 2)) != invalid) {
                            throw new IllegalStateException(charset + " plane " + plane + " is partly invalid");
                        }
                    }
                    if (invalid) {
                        planes[plane] = ERROR | 2;
                    } else {
                        planes[plane] = planeTables.size();
                        planeTables.add(table);
                    }
                }
                tables.add(planes);
                tables.addAll(planeTables);
            }
            case GB18030 -> tables.add(gb18030FourByteBmp(charset));
            default -> {
            }
        }
        return tables;
    }

    /** The cells of every two bytes following a prefix that needs them all. */
    private static int[] longer(Charset charset, int... prefix) {
        final int[] table = new int[256 * 256];
        final int[] sequence = new int[prefix.length + 2];
        System.arraycopy(prefix, 0, sequence, 0, prefix.length);
        for (int b1 = 0; b1 < 256; b1++) {
            for (int b2 = 0; b2 < 256; b2++) {
                sequence[prefix.length] = b1;
                sequence[prefix.length + 1] = b2;
                table[b1 * 256 + b2] = cell(charset, sequence);
                if (table[b1 * 256 + b2] == MORE) {
                    throw new IllegalStateException(charset + " needs more than " + sequence.length + " bytes");
                }
            }
        }
        return table;
    }

    /**
     * The characters of GB18030's four-byte sequences for the Basic Multilingual Plane, by their
     * linear index, which the decoder's arithmetic maps outside it.
     */
    private static int[] gb18030FourByteBmp(Charset charset) {
        final int[] table = new int[0x99FC];
        for (int index = 0; index < table.length; index++) {
            final int b3 = 0x30 + index % 10;
            final int b2 = 0x81 + index / 10 % 126;
            final int b1 = 0x30 + index / 1260 % 10;
            final int b0 = 0x81 + index / 12600;
            table[index] = cell(charset, b0, b1, b2, b3);
            if (table[index] < 0 || table[index] > 0xFFFF) {
                throw new IllegalStateException("GB18030 index " + index + " decodes to cell " + table[index]);
            }
        }
        return table;
    }

    /**
     * What the strict decoder makes of exactly these bytes, as the input so far: one character
     * taking them all, an error of a length, or the need for more bytes.
     */
    private static int cell(Charset charset, int... sequence) {
        final byte[] input = new byte[sequence.length];
        for (int i = 0; i < sequence.length; i++) {
            input[i] = (byte) sequence[i];
        }
        final CharsetDecoder decoder = charset.newDecoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT);
        final ByteBuffer in = ByteBuffer.wrap(input);
        final CharBuffer out = CharBuffer.allocate(4);
        final CoderResult result = decoder.decode(in, out, false);
        if (result.isError()) {
            if (in.position() != 0) {
                throw new IllegalStateException(charset + " fails after a character in " + hex(input));
            }
            return ERROR | result.length();
        }
        if (in.position() == 0) {
            return MORE;
        }
        out.flip();
        final int codePoint = Character.codePointAt(out, 0);
        if (in.position() != input.length || Character.charCount(codePoint) != out.length()) {
            throw new IllegalStateException(charset + " decodes " + hex(input) + " to more than one character");
        }
        return codePoint;
    }

    private static String hex(byte[] bytes) {
        final StringBuilder text = new StringBuilder();
        for (byte b : bytes) {
            text.append(String.format("%02X", b & 0xFF));
        }
        return text.toString();
    }
}

/**
 * Writes what the JDK's East Asian multi-byte decoders make of pseudo-random bytes, for the Rust
 * port of them to be tested against: per charset, a hash over the inputs of the text
 * {@code new String(bytes, charset)} gives and of what the strict decoder reports for the first
 * character at each position, as Tika's CJK decode check asks it. The Rust test draws the same
 * inputs from the same generator.
 */
final class DecoderReference {

    static final int INPUTS = 4000;
    static final int MAX_LENGTH = 16;
    static final int ERROR = 0xFFFF;
    static final long FNV_OFFSET = 0xcbf29ce484222325L;
    static final long FNV_PRIME = 0x100000001b3L;

    static long hash(Charset charset) {
        return hash(charset, true);
    }

    /** The hash of the decoded texts alone, for a charset without a CJK decode check. */
    static long textHash(Charset charset) {
        return hash(charset, false);
    }

    private static long hash(Charset charset, boolean firstCharacters) {
        final long[] state = {0x5EED};
        long hash = FNV_OFFSET;
        for (int n = 0; n < INPUTS; n++) {
            final byte[] input = new byte[1 + (int) Long.remainderUnsigned(next(state), MAX_LENGTH)];
            for (int i = 0; i < input.length; i++) {
                final long r = next(state);
                input[i] = (byte) (Long.remainderUnsigned(r, 4) == 0 ? (r >>> 8) & 0x7F : 0x80 | (r >>> 8) & 0x7F);
            }
            for (char unit : new String(input, charset).toCharArray()) {
                hash = (hash ^ unit) * FNV_PRIME;
            }
            hash = (hash ^ ERROR) * FNV_PRIME;
            for (int i = 0; firstCharacters && i < input.length; i++) {
                hash = (hash ^ decodeFirst(charset, input, i)) * FNV_PRIME;
            }
        }
        return hash;
    }

    /** The escapes and shifts of the ISO-2022 decoders, known and unknown, whole and cut short. */
    static final String[] ISO_2022_TOKENS = {
        "\u001b$)C", "\u001b$)A", "\u001b$)G", "\u001b$)X", "\u001b$A", "\u001b$*H",
        "\u001b$+I", "\u001b$*X", "\u001bN", "\u001bO", "\u001b", "\u001b$", "\u001b$)",
        "\u000e", "\u000f", "\n",
    };
    static final int ISO_2022_INPUTS = 20000;
    static final int ISO_2022_MAX_TOKENS = 24;

    /**
     * The hash of the texts an ISO-2022 decoder makes of inputs of tokens: an escape or shift of
     * {@link #ISO_2022_TOKENS}, mostly bytes that a shift-out pairs into characters, and now and
     * then a byte with its high bit set or any 7-bit byte.
     */
    static long iso2022Hash(Charset charset) {
        final long[] state = {0x2022};
        long hash = FNV_OFFSET;
        for (int n = 0; n < ISO_2022_INPUTS; n++) {
            final ByteArrayOutputStream input = new ByteArrayOutputStream();
            final long tokens = 1 + Long.remainderUnsigned(next(state), ISO_2022_MAX_TOKENS);
            for (long t = 0; t < tokens; t++) {
                final long r = next(state);
                final int kind = (int) Long.remainderUnsigned(r, 32);
                final long rest = r >>> 8;
                if (kind < ISO_2022_TOKENS.length) {
                    input.writeBytes(ISO_2022_TOKENS[kind].getBytes(StandardCharsets.ISO_8859_1));
                } else if (kind < 30) {
                    input.write(0x21 + (int) Long.remainderUnsigned(rest, 94));
                } else if (kind == 30) {
                    input.write(0x80 | (int) (rest & 0x7F));
                } else {
                    input.write((int) (rest & 0x7F));
                }
            }
            for (char unit : new String(input.toByteArray(), charset).toCharArray()) {
                hash = (hash ^ unit) * FNV_PRIME;
            }
            hash = (hash ^ ERROR) * FNV_PRIME;
        }
        return hash;
    }

    /** What {@code CjkDecodeValidator} asks of the decoder at a position. */
    static int decodeFirst(Charset charset, byte[] input, int at) {
        final CharsetDecoder decoder = charset.newDecoder()
                .onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT);
        final ByteBuffer in = ByteBuffer.wrap(input, at, Math.min(4, input.length - at));
        final CoderResult result = decoder.decode(in, CharBuffer.allocate(1), true);
        return result.isError() ? ERROR : in.position() - at;
    }

    /** SplitMix64. */
    static long next(long[] state) {
        long z = state[0] += 0x9E3779B97F4A7C15L;
        z = (z ^ (z >>> 30)) * 0xBF58476D1CE4E5B9L;
        z = (z ^ (z >>> 27)) * 0x94D049BB133111EBL;
        return z ^ (z >>> 31);
    }
}
