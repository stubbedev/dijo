package ai.yobix;

import org.apache.tika.exception.UnsupportedFormatException;

import java.io.IOException;
import java.io.OutputStream;
import java.io.Writer;
import java.nio.channels.FileChannel;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardOpenOption;
import java.util.List;
import java.util.zip.CRC32;

/**
 * Writes how our Java DjvuRenderer renders each page of the DjVu documents named after the
 * output file, paths relative to the repository: a line of the document, the page counted from
 * 1, and the rendering's size and the CRC-32 of its grey pixels, or the failure it ends with. A
 * page with a JPEG layer, which ImageIO's JPEG decoder decodes, is also written as a PGM file
 * beside the output, named in a fourth column, since another decoder rounds otherwise.
 */
public final class DjvuReference {

    private DjvuReference() {
    }

    public static void main(String[] args) throws IOException {
        final Path output = Path.of(args[0]);
        try (Writer out = Files.newBufferedWriter(output, StandardCharsets.UTF_8)) {
            out.write("file\tpage\trendered\tpixels\n");
            for (int i = 1; i < args.length; i++) {
                try (FileChannel file = FileChannel.open(Path.of(args[i]),
                        StandardOpenOption.READ)) {
                    final DjvuDocument document = DjvuDocument.read(file);
                    final List<DjvuDocument.Form> pages = document.pages();
                    for (int page = 0; page < pages.size(); page++) {
                        out.write(args[i] + "\t" + (page + 1) + "\t"
                                + rendered(document, pages.get(page), output, args[i], page + 1)
                                + "\n");
                    }
                } catch (Exception e) {
                    throw new IOException("Reading " + args[i] + " failed", e);
                }
            }
        }
    }

    private static String rendered(DjvuDocument document, DjvuDocument.Form page, Path output,
                                   String name, int number) {
        try {
            final Iw44Decoder.GreyImage image = DjvuRenderer.page(document, page);
            final CRC32 crc = new CRC32();
            crc.update(image.pixels());
            String pixels = "";
            if (hasJpegLayer(document, page)) {
                final String file = Path.of(name).getFileName().toString()
                        .replace(".djvu", "-" + number + ".pgm");
                try (OutputStream pgm = Files.newOutputStream(output.resolveSibling(file))) {
                    pgm.write(("P5\n" + image.width() + " " + image.height() + "\n255\n")
                            .getBytes(StandardCharsets.US_ASCII));
                    pgm.write(image.pixels());
                }
                pixels = file;
            }
            return image.width() + "x" + image.height() + " "
                    + String.format("%08x", crc.getValue()) + "\t" + pixels;
        } catch (UnsupportedFormatException e) {
            return "failed: UnsupportedFormatException: " + e.getMessage() + "\t";
        } catch (Exception e) {
            return "failed: " + e.getMessage() + "\t";
        }
    }

    private static boolean hasJpegLayer(DjvuDocument document, DjvuDocument.Form page)
            throws Exception {
        for (final DjvuDocument.Chunk chunk : document.chunks(page)) {
            if ("BGjp".equals(chunk.id()) || "FGjp".equals(chunk.id())) {
                return true;
            }
        }
        return false;
    }
}
