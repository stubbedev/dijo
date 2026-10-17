import javax.imageio.ImageIO;
import javax.imageio.ImageReader;
import javax.imageio.stream.ImageInputStream;
import java.awt.Graphics2D;
import java.awt.RenderingHints;
import java.awt.image.BufferedImage;
import java.awt.image.Raster;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.Collections;
import java.util.HexFormat;
import java.util.Iterator;
import java.util.List;
import java.util.Locale;
import java.util.Set;
import java.util.stream.Stream;
import java.util.zip.GZIPOutputStream;
import java.util.zip.ZipEntry;
import java.util.zip.ZipInputStream;

/**
 * Writes the grey pixels our Java PreparingOcrParser starts preparing each image from: the
 * first image ImageIO's reader for it decodes, drawn into grey as OcrImages.gray draws it. Every
 * JPEG, PNG, GIF and BMP file of the given corpora is read, and every one stored in a ZIP-based
 * file of them, named {@code label/path} or {@code label/path!entry}. Each line holds the name,
 * then the width, the height and the SHA-256 of the grey pixels row by row, or
 * {@code undecodable} where ImageIO fails.
 */
public final class ImageReference {

    private static final Set<String> EXTENSIONS = Set.of("jpg", "jpeg", "png", "gif", "bmp");
    private static final byte[] ZIP = {'P', 'K', 3, 4};

    public static void main(String[] args) throws Exception {
        if (args.length < 2) {
            System.err.println("usage: ImageReference <output> <label=dir>...");
            System.exit(2);
        }
        final List<String> lines = new ArrayList<>();
        for (int i = 1; i < args.length; i++) {
            final String[] corpus = args[i].split("=", 2);
            final Path root = Path.of(corpus[1]);
            final List<Path> files;
            try (Stream<Path> walk = Files.walk(root)) {
                files = walk.filter(Files::isRegularFile).sorted().toList();
            }
            for (Path file : files) {
                final String name = corpus[0] + "/" + root.relativize(file).toString()
                        .replace('\\', '/');
                final byte[] bytes = Files.readAllBytes(file);
                if (isImage(name)) {
                    lines.add(line(name, bytes));
                } else if (startsWith(bytes, ZIP)) {
                    lines.addAll(entries(name, bytes));
                }
            }
        }
        Collections.sort(lines);
        final Path output = Path.of(args[0]);
        final Path temporary = output.resolveSibling(output.getFileName() + ".tmp");
        try (OutputStream out = new GZIPOutputStream(Files.newOutputStream(temporary))) {
            for (String line : lines) {
                out.write((line + "\n").getBytes(StandardCharsets.UTF_8));
            }
        }
        Files.move(temporary, output, StandardCopyOption.REPLACE_EXISTING,
                StandardCopyOption.ATOMIC_MOVE);
        System.err.println("ImageReference: " + lines.size() + " images");
    }

    private static boolean isImage(String name) {
        final int dot = name.lastIndexOf('.');
        return dot >= 0 && EXTENSIONS.contains(name.substring(dot + 1).toLowerCase(Locale.ROOT));
    }

    private static boolean startsWith(byte[] bytes, byte[] prefix) {
        if (bytes.length < prefix.length) {
            return false;
        }
        for (int i = 0; i < prefix.length; i++) {
            if (bytes[i] != prefix[i]) {
                return false;
            }
        }
        return true;
    }

    /** The lines of the images a ZIP-based file stores, or none when it cannot be read. */
    private static List<String> entries(String name, byte[] bytes) throws Exception {
        final List<String> lines = new ArrayList<>();
        try (ZipInputStream zip = new ZipInputStream(new ByteArrayInputStream(bytes))) {
            ZipEntry entry;
            while ((entry = zip.getNextEntry()) != null) {
                if (!entry.isDirectory() && isImage(entry.getName())) {
                    lines.add(line(name + "!" + entry.getName(), zip.readAllBytes()));
                }
            }
        } catch (IOException | IllegalArgumentException e) {
            return lines;
        }
        return lines;
    }

    private static String line(String name, byte[] bytes) throws Exception {
        final BufferedImage image = read(bytes);
        if (image == null) {
            return name + "\tundecodable";
        }
        final BufferedImage gray = gray(image, image.getWidth(), image.getHeight());
        final MessageDigest digest = MessageDigest.getInstance("SHA-256");
        final Raster raster = gray.getRaster();
        final byte[] row = new byte[gray.getWidth()];
        for (int y = 0; y < gray.getHeight(); y++) {
            raster.getDataElements(0, y, gray.getWidth(), 1, row);
            digest.update(row);
        }
        return name + "\t" + gray.getWidth() + "\t" + gray.getHeight() + "\t"
                + HexFormat.of().formatHex(digest.digest());
    }

    /** The first image of a file, as PreparingOcrParser's Pages reads it, or null. */
    private static BufferedImage read(byte[] bytes) {
        try (ImageInputStream input =
                     ImageIO.createImageInputStream(new ByteArrayInputStream(bytes))) {
            if (input == null) {
                return null;
            }
            final Iterator<ImageReader> readers = ImageIO.getImageReaders(input);
            if (!readers.hasNext()) {
                return null;
            }
            final ImageReader reader = readers.next();
            try {
                reader.setInput(input, false, true);
                return reader.read(0);
            } finally {
                reader.dispose();
            }
        } catch (Exception | LinkageError e) {
            return null;
        }
    }

    /** OcrImages.gray. */
    private static BufferedImage gray(BufferedImage image, int width, int height) {
        if (image.getType() == BufferedImage.TYPE_BYTE_GRAY && image.getWidth() == width
                && image.getHeight() == height) {
            return image;
        }
        final BufferedImage gray =
                new BufferedImage(width, height, BufferedImage.TYPE_BYTE_GRAY);
        final Graphics2D graphics = gray.createGraphics();
        try {
            graphics.setRenderingHint(RenderingHints.KEY_INTERPOLATION,
                    RenderingHints.VALUE_INTERPOLATION_BILINEAR);
            graphics.drawImage(image, 0, 0, width, height, null);
        } finally {
            graphics.dispose();
        }
        return gray;
    }
}
