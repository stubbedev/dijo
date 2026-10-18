import java.awt.image.BufferedImage;
import java.awt.image.DataBufferByte;
import java.io.PrintWriter;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.security.MessageDigest;
import java.util.HexFormat;
import java.util.Iterator;
import javax.imageio.ImageIO;
import javax.imageio.ImageReader;
import javax.imageio.stream.ImageInputStream;

/**
 * Writes the rasters PDFBox's {@code JBIG2Filter} takes of JBIG2 images, as the PDFBox JBIG2
 * plugin of Tika's class path reads them: for each file, its name, the image's width and height,
 * and the length and MD5 of the bytes of its raster, rows of 1-bit samples.
 * <p>
 * Usage: {@code java -cp <pdfbox jars> Jbig2Rasters.java <output.tsv> <file.jb2>...}
 */
public final class Jbig2Rasters {

    public static void main(String[] args) throws Exception {
        if (args.length < 2) {
            System.err.println("usage: Jbig2Rasters <output.tsv> <file.jb2>...");
            System.exit(2);
        }
        final Path output = Path.of(args[0]);
        final Path temporary = output.resolveSibling(output.getFileName() + ".tmp");
        try (PrintWriter out = new PrintWriter(Files.newBufferedWriter(temporary,
                StandardCharsets.UTF_8))) {
            for (int i = 1; i < args.length; i++) {
                final Path file = Path.of(args[i]);
                final Iterator<ImageReader> readers = ImageIO.getImageReadersByFormatName("JBIG2");
                final ImageReader reader = readers.next();
                try (ImageInputStream input = ImageIO.createImageInputStream(file.toFile())) {
                    reader.setInput(input);
                    final BufferedImage image = reader.read(0, reader.getDefaultReadParam());
                    final byte[] raster =
                            ((DataBufferByte) image.getData().getDataBuffer()).getData();
                    final byte[] md5 = MessageDigest.getInstance("MD5").digest(raster);
                    out.printf("%s\t%d\t%d\t%d\t%s%n", file.getFileName(), image.getWidth(),
                            image.getHeight(), raster.length, HexFormat.of().formatHex(md5));
                } finally {
                    reader.dispose();
                }
            }
        }
        Files.move(temporary, output, StandardCopyOption.REPLACE_EXISTING,
                StandardCopyOption.ATOMIC_MOVE);
    }
}
