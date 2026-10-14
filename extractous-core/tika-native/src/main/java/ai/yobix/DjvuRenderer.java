package ai.yobix;

import org.apache.tika.exception.TikaException;
import org.apache.tika.exception.UnsupportedFormatException;
import org.apache.tika.io.TemporaryResources;

import javax.imageio.ImageIO;
import java.awt.image.BufferedImage;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

/**
 * DjVu pages rendered in grey for OCR (DjVu v3 specification, section 4): the JB2 mask's pixels
 * in the foreground's colour, from the FG44 image or the FGbz palette, black without either, and
 * every other pixel in the background's, from the BG44 image or a BGjp JPEG, white without
 * either; then turned as the INFO chunk's orientation says.
 */
final class DjvuRenderer {

    private static final int MAX_INCLUDES = 16;
    private static final int MAX_REDUCTION = 12;
    private static final int FGBZ_INDICES = 0x80;
    private static final int MAX_PALETTE_BYTES = 16 << 20;

    private DjvuRenderer() {
    }

    /** A page, counted from 1, rendered at full resolution as a PGM file. */
    static Path render(DjvuDocument document, int page, TemporaryResources temporary)
            throws IOException, TikaException {
        final Iw44Decoder.GreyImage image = page(document, document.pages().get(page - 1));
        final Path pgm = temporary.createTempFile(".pgm");
        try (OutputStream out = Files.newOutputStream(pgm)) {
            out.write(("P5\n" + image.width() + " " + image.height() + "\n255\n")
                    .getBytes(StandardCharsets.US_ASCII));
            out.write(image.pixels());
        }
        return pgm;
    }

    /** The page a FORM:DJVU chunk codes, in grey. */
    static Iw44Decoder.GreyImage page(DjvuDocument document, DjvuDocument.Form page)
            throws IOException, TikaException {
        final List<DjvuDocument.Chunk> chunks = document.chunks(page);
        DjvuDocument.Chunk info = null;
        DjvuDocument.Chunk mask = null;
        DjvuDocument.Chunk palette = null;
        DjvuDocument.Chunk foregroundJpeg = null;
        DjvuDocument.Chunk backgroundJpeg = null;
        final List<byte[]> foreground = new ArrayList<>();
        final List<byte[]> background = new ArrayList<>();
        for (final DjvuDocument.Chunk chunk : chunks) {
            switch (chunk.id()) {
                case "INFO" -> info = chunk;
                case "Sjbz" -> mask = chunk;
                case "Smmr" -> throw new UnsupportedFormatException(
                        "DjVu masks coded with MMR (Smmr) are not supported");
                case "FGbz" -> palette = chunk;
                case "FG44" -> foreground.add(document.data(chunk));
                case "BG44" -> background.add(document.data(chunk));
                case "FGjp" -> foregroundJpeg = chunk;
                case "BGjp" -> backgroundJpeg = chunk;
                default -> {
                }
            }
        }
        if (info == null || info.size() < 4) {
            throw new TikaException("A DjVu page has no INFO chunk");
        }
        final byte[] infoData = document.data(info);
        final int width = (infoData[0] & 0xff) << 8 | (infoData[1] & 0xff);
        final int height = (infoData[2] & 0xff) << 8 | (infoData[3] & 0xff);
        final int orientation = infoData.length > 9 ? infoData[9] & 0x07 : 1;
        if ((long) width * height > Jb2Decoder.MAX_PIXELS) {
            throw new TikaException("A DjVu page of " + width + "×" + height + " pixels exceeds "
                    + Jb2Decoder.MAX_PIXELS);
        }
        final Iw44Decoder.GreyImage back = !background.isEmpty() ? Iw44Decoder.decode(background)
                : backgroundJpeg != null ? jpeg(document.data(backgroundJpeg)) : null;
        final byte[] pixels = new byte[width * height];
        paint(pixels, width, height, back, (byte) 0xff);
        if (mask != null) {
            final Jb2Decoder.Mask decoded = Jb2Decoder.decodeImage(document.data(mask),
                    dictionary(document, chunks, 0));
            if (decoded.image().width() != width || decoded.image().height() != height) {
                throw new TikaException("A DjVu page of " + width + "×" + height
                        + " has a mask of " + decoded.image().width() + "×"
                        + decoded.image().height());
            }
            final Iw44Decoder.GreyImage front = !foreground.isEmpty()
                    ? Iw44Decoder.decode(foreground)
                    : foregroundJpeg != null ? jpeg(document.data(foregroundJpeg)) : null;
            if (palette != null) {
                paintBlits(pixels, width, height, decoded.blits(), document.data(palette));
            } else {
                final byte[] colour = new byte[width * height];
                paint(colour, width, height, front, (byte) 0);
                final byte[] black = decoded.image().pixels();
                for (int i = 0; i < pixels.length; i++) {
                    if (black[i] != 0) {
                        pixels[i] = colour[i];
                    }
                }
            }
        }
        return oriented(new Iw44Decoder.GreyImage(width, height, pixels), orientation);
    }

    /**
     * The shapes of the shared dictionary a page's mask may require: its own Djbz chunk, or the
     * one a component file it includes holds, itself completed by the files that one includes.
     */
    private static List<Jb2Decoder.Bitmap> dictionary(DjvuDocument document,
                                                      List<DjvuDocument.Chunk> chunks, int depth)
            throws IOException, TikaException {
        if (depth > MAX_INCLUDES) {
            throw new TikaException("DjVu component files include each other more than "
                    + MAX_INCLUDES + " deep");
        }
        List<Jb2Decoder.Bitmap> included = null;
        DjvuDocument.Chunk own = null;
        for (final DjvuDocument.Chunk chunk : chunks) {
            if ("Djbz".equals(chunk.id())) {
                own = chunk;
            } else if ("INCL".equals(chunk.id()) && included == null) {
                final String id = new String(document.data(chunk), StandardCharsets.UTF_8).strip();
                included = dictionary(document, document.chunks(document.component(id)),
                        depth + 1);
            }
        }
        if (own != null) {
            return Jb2Decoder.decodeDictionary(document.data(own), included);
        }
        return included;
    }

    /** Fills a page-sized image from one of the same aspect at a whole fraction of its size. */
    private static void paint(byte[] pixels, int width, int height, Iw44Decoder.GreyImage layer,
                              byte missing) throws TikaException {
        if (layer == null) {
            Arrays.fill(pixels, missing);
            return;
        }
        final int reduction = (width + layer.width() - 1) / Math.max(1, layer.width());
        if (reduction < 1 || reduction > MAX_REDUCTION
                || (height + reduction - 1) / reduction != layer.height()) {
            throw new TikaException("A DjVu layer of " + layer.width() + "×" + layer.height()
                    + " does not fit a page of " + width + "×" + height);
        }
        for (int y = 0; y < height; y++) {
            final int row = (y / reduction) * layer.width();
            for (int x = 0; x < width; x++) {
                pixels[y * width + x] = layer.pixels()[row + x / reduction];
            }
        }
    }

    /**
     * Paints each symbol the mask places in its colour from an FGbz chunk (8.3.10): a palette of
     * BGR colours, then, when the version's high bit says so, a BZZ-coded colour index for each
     * placed symbol in order; without indices every symbol takes the first colour.
     */
    private static void paintBlits(byte[] pixels, int width, int height,
                                   List<Jb2Decoder.Blit> blits, byte[] chunk)
            throws TikaException {
        if (chunk.length < 3) {
            throw new TikaException("A DjVu FGbz chunk ends inside its header");
        }
        final int colours = (chunk[1] & 0xff) << 8 | (chunk[2] & 0xff);
        final int indexStart = 3 + 3 * colours;
        if (colours == 0 || indexStart > chunk.length) {
            throw new TikaException("A DjVu FGbz chunk ends inside its palette of " + colours);
        }
        final byte[] greys = new byte[colours];
        for (int i = 0; i < colours; i++) {
            final int blue = chunk[3 + 3 * i] & 0xff;
            final int green = chunk[4 + 3 * i] & 0xff;
            final int red = chunk[5 + 3 * i] & 0xff;
            greys[i] = grey(red, green, blue);
        }
        int[] indices = null;
        if ((chunk[0] & FGBZ_INDICES) != 0) {
            if (indexStart + 3 > chunk.length) {
                throw new TikaException("A DjVu FGbz chunk ends inside its index count");
            }
            final int count = (chunk[indexStart] & 0xff) << 16
                    | (chunk[indexStart + 1] & 0xff) << 8 | (chunk[indexStart + 2] & 0xff);
            final byte[] coded = BzzDecoder.decode(Arrays.copyOfRange(chunk,
                    indexStart + 3, chunk.length), MAX_PALETTE_BYTES);
            if (coded.length < 2 * count) {
                throw new TikaException("A DjVu FGbz chunk has fewer colour indices than "
                        + count);
            }
            indices = new int[count];
            for (int i = 0; i < count; i++) {
                indices[i] = (coded[2 * i] & 0xff) << 8 | (coded[2 * i + 1] & 0xff);
            }
        }
        for (int b = 0; b < blits.size(); b++) {
            final int index = indices == null ? 0 : b < indices.length ? indices[b] : -1;
            if (index < 0 || index >= colours) {
                throw new TikaException("A DjVu symbol has no colour in its FGbz palette");
            }
            final Jb2Decoder.Blit blit = blits.get(b);
            final Jb2Decoder.Bitmap mark = blit.mark();
            for (int y = Math.max(0, -blit.y()); y < mark.height() && blit.y() + y < height; y++) {
                for (int x = Math.max(0, -blit.x()); x < mark.width() && blit.x() + x < width;
                     x++) {
                    if (mark.pixels()[y * mark.width() + x] != 0) {
                        pixels[(blit.y() + y) * width + blit.x() + x] = greys[index];
                    }
                }
            }
        }
    }

    private static Iw44Decoder.GreyImage jpeg(byte[] data) throws IOException, TikaException {
        final BufferedImage image = ImageIO.read(new ByteArrayInputStream(data));
        if (image == null) {
            throw new TikaException("A DjVu JPEG layer cannot be decoded");
        }
        final int width = image.getWidth();
        final int height = image.getHeight();
        final byte[] pixels = new byte[width * height];
        for (int y = 0; y < height; y++) {
            for (int x = 0; x < width; x++) {
                final int rgb = image.getRGB(x, y);
                pixels[y * width + x] = grey((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff);
            }
        }
        return new Iw44Decoder.GreyImage(width, height, pixels);
    }

    /** A colour's grey, weighted as DjVuLibre's renderer weighs it. */
    private static byte grey(int red, int green, int blue) {
        return (byte) ((5 * red + 9 * green + 2 * blue) >> 4);
    }

    /**
     * The page turned as its INFO chunk's orientation says (8.3.11): 6 a quarter turn
     * counter-clockwise, 2 half a turn, 5 a quarter turn clockwise, and any other value upright.
     */
    private static Iw44Decoder.GreyImage oriented(Iw44Decoder.GreyImage page, int orientation) {
        final int width = page.width();
        final int height = page.height();
        final byte[] source = page.pixels();
        return switch (orientation) {
            case 2 -> {
                final byte[] turned = new byte[source.length];
                for (int i = 0; i < source.length; i++) {
                    turned[source.length - 1 - i] = source[i];
                }
                yield new Iw44Decoder.GreyImage(width, height, turned);
            }
            case 5, 6 -> {
                final byte[] turned = new byte[source.length];
                for (int y = 0; y < height; y++) {
                    for (int x = 0; x < width; x++) {
                        final int target = orientation == 6
                                ? (width - 1 - x) * height + y
                                : x * height + (height - 1 - y);
                        turned[target] = source[y * width + x];
                    }
                }
                yield new Iw44Decoder.GreyImage(height, width, turned);
            }
            default -> page;
        };
    }
}
