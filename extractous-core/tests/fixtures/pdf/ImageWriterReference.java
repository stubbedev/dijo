import java.awt.image.BufferedImage;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.PrintWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;
import java.util.HexFormat;
import java.util.zip.Deflater;

import org.apache.pdfbox.tools.imageio.ImageIOUtil;

/**
 * Writes what the JDK's writers make of generated data, as the Rust ports of them are tested
 * against: the zlib streams {@code Deflater} at its best compression makes of inputs fed in
 * pieces, and the PNG files PDFBox's {@code ImageIOUtil} writes of grey, RGB and ARGB images,
 * each by its length and SHA-256.
 * <p>
 * Usage: {@code java -cp <pdfbox and pdfbox-tools jars> ImageWriterReference.java <output.tsv>}
 */
public final class ImageWriterReference {

    private static final int[] LENGTHS = {0, 1, 100, 5000, 70000, 300000};
    private static final int[] PIECES = {0, 997};
    private static final int[][] SIZES = {{1, 1}, {3, 2}, {17, 9}, {300, 200}};

    /** A generator of the data, which the Rust test repeats. */
    private static final class Lcg {
        private long state;

        Lcg(long seed) {
            state = seed;
        }

        long next() {
            state = state * 6364136223846793005L + 1442695040888963407L;
            return state;
        }
    }

    /** Input of one of four kinds: noise, few values, runs, or words. */
    static byte[] input(int kind, int length) {
        Lcg lcg = new Lcg(kind + 1);
        byte[] data = new byte[length];
        for (int i = 0; i < length; i++) {
            long x = lcg.next();
            switch (kind) {
                case 0 -> data[i] = (byte) (x >>> 56);
                case 1 -> data[i] = (byte) (x >>> 61);
                case 2 -> data[i] = (byte) ((i / 300) % 7 * 30);
                default -> {
                    int letter = (int) ((x >>> 58) % 27);
                    data[i] = (byte) (letter == 26 ? ' ' : 'a' + letter % 6);
                }
            }
        }
        return data;
    }

    /**
     * Deflates {@code data}, given whole, or in pieces of {@code piece} bytes, each given as
     * its first byte and the rest, as {@code PNGImageWriter} gives each row.
     */
    static byte[] deflate(byte[] data, int piece) {
        Deflater deflater = new Deflater(Deflater.BEST_COMPRESSION);
        ByteArrayOutputStream out = new ByteArrayOutputStream();
        byte[] buffer = new byte[512];
        int at = 0;
        boolean head = true;
        while (at < data.length) {
            int length = piece == 0 ? data.length : Math.min(head ? 1 : piece - 1, data.length - at);
            head = !head;
            deflater.setInput(data, at, length);
            while (!deflater.needsInput()) {
                out.write(buffer, 0, deflater.deflate(buffer));
            }
            at += length;
        }
        deflater.finish();
        while (!deflater.finished()) {
            out.write(buffer, 0, deflater.deflate(buffer));
        }
        deflater.end();
        return out.toByteArray();
    }

    /** An image of `type`, its samples from the generator. */
    static BufferedImage image(int type, int width, int height) {
        Lcg lcg = new Lcg(type * 1000L + width);
        BufferedImage image = new BufferedImage(width, height, type);
        for (int y = 0; y < height; y++) {
            for (int x = 0; x < width; x++) {
                long sample = lcg.next() >>> 32;
                int argb = (int) (sample & (x % 7 == 0 ? 0xffffffffL : 0x3f3f3f3fL));
                if (type == BufferedImage.TYPE_BYTE_GRAY) {
                    image.getRaster().setSample(x, y, 0, argb & 0xff);
                } else {
                    image.setRGB(x, y, argb);
                }
            }
        }
        return image;
    }

    public static void main(String[] args) throws IOException, NoSuchAlgorithmException {
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(Path.of(args[0]),
                StandardCharsets.UTF_8))) {
            out.print("# deflate\tkind\tlength\tpiece\tbytes\tsha-256; "
                    + "png\ttype\twidth\theight\tbytes\tsha-256\n");
            for (int kind = 0; kind < 4; kind++) {
                for (int length : LENGTHS) {
                    for (int piece : PIECES) {
                        byte[] deflated = deflate(input(kind, length), piece);
                        out.print("deflate\t" + kind + "\t" + length + "\t" + piece + "\t"
                                + deflated.length + "\t" + sha256(deflated) + "\n");
                    }
                }
            }
            int[] types = {BufferedImage.TYPE_BYTE_GRAY, BufferedImage.TYPE_INT_RGB,
                    BufferedImage.TYPE_INT_ARGB};
            for (int type : types) {
                for (int[] size : SIZES) {
                    ByteArrayOutputStream png = new ByteArrayOutputStream();
                    ImageIOUtil.writeImage(image(type, size[0], size[1]), "png", png);
                    byte[] bytes = png.toByteArray();
                    out.print("png\t" + type + "\t" + size[0] + "\t" + size[1] + "\t"
                            + bytes.length + "\t" + sha256(bytes) + "\n");
                }
            }
        }
    }

    private static String sha256(byte[] data) throws NoSuchAlgorithmException {
        return HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256").digest(data));
    }
}
