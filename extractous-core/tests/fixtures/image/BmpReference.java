import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.PrintWriter;
import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;
import java.util.LinkedHashMap;
import java.util.Map;

import org.apache.tika.metadata.Metadata;
import org.apache.tika.parser.ParseContext;
import org.apache.tika.parser.image.ImageParser;
import org.apache.tika.sax.BodyContentHandler;
import org.xml.sax.ContentHandler;

/**
 * Writes what Tika's {@link ImageParser} reads of BMP and WBMP images through the JDK's own
 * ImageIO readers, as it does with no other reader on the class path: Tika's native image also
 * holds jai-imageio's readers and picks one of the two at random in each process (#114), which
 * the native parser does not.
 * <p>
 * It writes the images it reads into {@code bmp/} beside its output first: a BMP of each header
 * version, depth, compression and palette the JDK reads, and some it fails on. Then
 * {@code bmp-reference.tsv}, for each image, the corpus's {@code testBMP.bmp} last, a line
 * {@code #<name>} and a line {@code <key>\t<values>} for each metadata key in sorted order, its
 * values joined by {@code |}, or a line {@code !<message>} with the message of the exception
 * the parser fails with, at its root. Tabs, line breaks and backslashes are escaped.
 * <p>
 * Usage: {@code java -cp <Tika's core and image module and their dependencies, no jai-imageio>
 * BmpReference.java <fixture directory> <corpus testBMP.bmp>}.
 */
public class BmpReference {

    public static void main(String[] args) throws Exception {
        Path fixtures = Path.of(args[0]);
        Path images = fixtures.resolve("bmp");
        Files.createDirectories(images);
        Map<String, byte[]> written = images();
        for (Map.Entry<String, byte[]> image : written.entrySet()) {
            Files.write(images.resolve(image.getKey()), image.getValue());
        }
        Map<String, byte[]> read = new LinkedHashMap<>(written);
        read.put("testBMP.bmp", Files.readAllBytes(Path.of(args[1])));
        StringBuilder out = new StringBuilder();
        for (Map.Entry<String, byte[]> image : read.entrySet()) {
            out.append('#').append(image.getKey()).append('\n');
            out.append(describe(image.getKey(), image.getValue()));
        }
        Files.writeString(fixtures.resolve("bmp-reference.tsv"), out.toString(),
                StandardCharsets.UTF_8);
    }

    private static String describe(String name, byte[] image) throws Exception {
        Metadata metadata = new Metadata();
        metadata.set("Content-Type", name.endsWith(".wbmp") ? "image/vnd.wap.wbmp" : "image/bmp");
        Method extract = ImageParser.class.getDeclaredMethod("extractMetadata",
                InputStream.class, ContentHandler.class, Metadata.class, ParseContext.class);
        extract.setAccessible(true);
        try {
            extract.invoke(new ImageParser(), new ByteArrayInputStream(image),
                    new BodyContentHandler(), metadata, new ParseContext());
        } catch (InvocationTargetException e) {
            Throwable root = e.getCause();
            while (root.getCause() != null) {
                root = root.getCause();
            }
            return "!" + escape(String.valueOf(root.getMessage())) + "\n";
        }
        StringBuilder lines = new StringBuilder();
        String[] names = metadata.names();
        Arrays.sort(names);
        for (String key : names) {
            lines.append(escape(key)).append('\t');
            String[] values = metadata.getValues(key);
            for (int i = 0; i < values.length; i++) {
                if (i > 0) {
                    lines.append('|');
                }
                lines.append(escape(values[i]));
            }
            lines.append('\n');
        }
        return lines.toString();
    }

    private static String escape(String text) {
        return text.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n")
                .replace("\r", "\\r");
    }

    /** The images, by name, each 2 by 2 pixels unless its name says otherwise. */
    private static Map<String, byte[]> images() throws IOException {
        Map<String, byte[]> images = new LinkedHashMap<>();
        byte[] two = {0, 0, 0, 0, (byte) 0xFF, (byte) 0xFF, (byte) 0xFF, 0};
        byte[] four = new byte[16 * 4];
        for (int i = 0; i < four.length; i++) {
            four[i] = (byte) (i * 37);
        }
        images.put("v3-24.bmp", bmp(40, 2, 2, 24, 0, 2835, 2835, 0, null, null, rows(2, 2, 24)));
        images.put("v3-8-aspect.bmp",
                bmp(40, 2, 2, 8, 0, 2835, 3780, 2, two, null, rows(2, 2, 8)));
        images.put("v3-1.bmp", bmp(40, 2, 2, 1, 0, 0, 0, 2, two, null, rows(2, 2, 1)));
        images.put("v3-4-rle4.bmp", bmp(40, 2, 2, 4, 2, 0, 0, 16, four, null,
                new byte[] {2, 0x12, 0, 0, 2, 0x34, 0, 1}));
        images.put("v3-8-rle8.bmp", bmp(40, 2, 2, 8, 1, 0, 0, 2, two, null,
                new byte[] {2, 1, 0, 0, 2, 0, 0, 1}));
        images.put("v3-16.bmp", bmp(40, 2, 2, 16, 0, 0, 0, 0, null, null, rows(2, 2, 16)));
        images.put("v3-32.bmp", bmp(40, 2, 2, 32, 0, 0, 0, 0, null, null, rows(2, 2, 32)));
        images.put("v3-16-565.bmp", bmp(40, 2, 2, 16, 3, 0, 0, 0, null,
                new int[] {0xF800, 0x07E0, 0x001F}, rows(2, 2, 16)));
        images.put("v3-32-fields-palette.bmp", bmp(40, 2, 2, 32, 3, 0, 0, 2, two,
                new int[] {0xFF0000, 0xFF00, 0xFF}, rows(2, 2, 32)));
        images.put("v2-8.bmp", core(8, new byte[] {0, 0, 0, (byte) 0xFF, (byte) 0xFF,
                (byte) 0xFF}, rows(2, 2, 8)));
        images.put("v4-32-alpha.bmp", bmp(108, 2, 2, 32, 3, 0, 0, 0, null,
                new int[] {0xFF0000, 0xFF00, 0xFF, 0xFF000000}, rows(2, 2, 32)));
        images.put("v5-24.bmp", bmp(124, 2, 2, 24, 0, 1000, 1000, 0, null, null, rows(2, 2, 24)));
        images.put("top-down.bmp", bmp(40, 2, -2, 24, 0, 0, 0, 0, null, null, rows(2, 2, 24)));
        byte[] alpha = two.clone();
        alpha[3] = 9;
        images.put("palette-alpha.bmp",
                bmp(40, 2, 2, 8, 0, 0, 0, 2, alpha, null, rows(2, 2, 8)));
        images.put("png-inside.bmp", bmp(40, 2, 2, 0, 5, 0, 0, 0, null, null, new byte[8]));
        byte[] magic = bmp(40, 2, 2, 24, 0, 0, 0, 0, null, null, rows(2, 2, 24));
        magic[0] = 'X';
        images.put("bad-magic.bmp", magic);
        images.put("two-bits.bmp", bmp(40, 2, 2, 2, 0, 0, 0, 2, two, null, rows(2, 2, 8)));
        byte[] short24 = bmp(40, 20, 20, 24, 0, 0, 0, 0, null, null, rows(2, 2, 24));
        images.put("short-data.bmp", short24);
        images.put("zero-width.bmp", bmp(40, 0, 2, 8, 0, 0, 0, 2, two, null, new byte[8]));
        images.put("compression-7.bmp", bmp(40, 2, 2, 24, 7, 0, 0, 0, null, null, rows(2, 2, 24)));
        images.put("unknown-header.bmp", bmp(64, 2, 2, 24, 0, 0, 0, 0, null, null, rows(2, 2, 24)));
        images.put("empty-palette.bmp", bmp(40, 2, 2, 8, 0, 0, 0, 0, null, null, rows(2, 2, 8)));
        images.put("split-mask.bmp", bmp(40, 2, 2, 16, 3, 0, 0, 0, null,
                new int[] {0xF00F, 0x00F0, 0x0F00}, rows(2, 2, 16)));
        images.put("v3-ext-56.bmp", bmp(56, 2, 2, 32, 3, 0, 0, 0, null,
                new int[] {0xFF0000, 0xFF00, 0xFF, 0xFF000000}, rows(2, 2, 32)));
        images.put("tiny.wbmp", new byte[] {0, 0, (byte) 0x88, 0x01, 2, (byte) 0xAA, 0x55});
        images.put("bad.wbmp", new byte[] {1, 0, 2, 2, 0});
        images.put("cut.wbmp", new byte[] {0, 0, (byte) 0x88});
        return images;
    }

    /** Pixel rows of `bits` per pixel, padded to four bytes, with samples that vary. */
    private static byte[] rows(int width, int height, int bits) {
        int stride = (width * bits + 31) / 32 * 4;
        byte[] data = new byte[stride * height];
        for (int i = 0; i < data.length; i++) {
            data[i] = (byte) (i * 29 + 7);
        }
        return data;
    }

    /** A BMP of a BITMAPINFOHEADER of `size` bytes, its masks after it where given. */
    private static byte[] bmp(int size, int width, int height, int bits, int compression,
                              int xPerMetre, int yPerMetre, int colours, byte[] palette,
                              int[] masks, byte[] pixels) throws IOException {
        ByteArrayOutputStream info = new ByteArrayOutputStream();
        ByteBuffer header = le(size);
        header.putInt(size).putInt(width).putInt(height).putShort((short) 1)
                .putShort((short) bits).putInt(compression).putInt(pixels.length)
                .putInt(xPerMetre).putInt(yPerMetre).putInt(colours).putInt(0);
        if (masks != null) {
            int fields = size == 40 ? 0 : 40;
            for (int mask : masks) {
                if (size == 40) {
                    continue;
                }
                header.putInt(fields, mask);
                fields += 4;
            }
        }
        info.write(header.array());
        if (masks != null && size == 40) {
            ByteBuffer fields = le(masks.length * 4);
            for (int mask : masks) {
                fields.putInt(mask);
            }
            info.write(fields.array());
        }
        if (palette != null) {
            info.write(palette);
        }
        return file(info.toByteArray(), pixels);
    }

    /** A BMP of a BITMAPCOREHEADER of `bits` per pixel and a palette of three-byte entries. */
    private static byte[] core(int bits, byte[] palette, byte[] pixels) throws IOException {
        ByteBuffer header = le(12);
        header.putInt(12).putShort((short) 2).putShort((short) 2).putShort((short) 1)
                .putShort((short) bits);
        ByteArrayOutputStream info = new ByteArrayOutputStream();
        info.write(header.array());
        info.write(palette);
        return file(info.toByteArray(), pixels);
    }

    /** The file of a header and palette `info` and its `pixels`. */
    private static byte[] file(byte[] info, byte[] pixels) {
        ByteBuffer file = le(14 + info.length + pixels.length);
        file.put((byte) 'B').put((byte) 'M').putInt(14 + info.length + pixels.length).putInt(0)
                .putInt(14 + info.length).put(info).put(pixels);
        return file.array();
    }

    private static ByteBuffer le(int size) {
        return ByteBuffer.allocate(size).order(ByteOrder.LITTLE_ENDIAN);
    }
}
