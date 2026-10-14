package ai.yobix;

import org.apache.tika.io.TemporaryResources;

import javax.imageio.ImageIO;
import java.awt.image.BufferedImage;
import java.io.IOException;
import java.nio.file.Path;

/** An image tesseract reads: a file it decodes itself, or pixels Java decoded and prepared. */
sealed interface OcrImage {

    /** The image as a file tesseract can decode: itself, or its pixels written as PNG. */
    Path file(TemporaryResources temporary) throws IOException;

    /** An image file, in a format tesseract decodes. */
    record File(Path path) implements OcrImage {

        @Override
        public Path file(TemporaryResources temporary) {
            return path;
        }
    }

    /** Decoded pixels. */
    record Pixels(BufferedImage image) implements OcrImage {

        @Override
        public Path file(TemporaryResources temporary) throws IOException {
            final Path path = temporary.createTempFile(".png");
            if (!ImageIO.write(image, "png", path.toFile())) {
                throw new IOException("No PNG writer to hand an image to tesseract");
            }
            return path;
        }

        boolean gray() {
            return image.getType() == BufferedImage.TYPE_BYTE_GRAY;
        }

        /** The grey level of each pixel, row by row; for a grey image only. */
        byte[] grayLevels() {
            final int width = image.getWidth();
            final int height = image.getHeight();
            final byte[] levels = new byte[width * height];
            final byte[] row = new byte[width];
            for (int y = 0; y < height; y++) {
                image.getRaster().getDataElements(0, y, width, 1, row);
                System.arraycopy(row, 0, levels, y * width, width);
            }
            return levels;
        }

        /** The packed RGB of each pixel, row by row. */
        int[] rgb() {
            return image.getRGB(0, 0, image.getWidth(), image.getHeight(), null, 0,
                    image.getWidth());
        }
    }
}
