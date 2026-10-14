package ai.yobix;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Path;

/**
 * libheif linked into the process, as the static build links it in place of the heif-dec program:
 * a call writes what the program writes.
 */
final class LinkedImages {

    private LinkedImages() {
    }

    /** Decodes a HEIC, HEIF or AVIF image to a PNG file, as {@code heif-dec} does. */
    static void decodeHeif(Path image, Path png) throws IOException {
        check(heifToPng(utf8(image), utf8(png)), "Decoding " + image + " for OCR");
    }

    private static void check(byte[] failure, String attempt) throws IOException {
        if (failure != null) {
            throw new IOException(attempt + " failed: "
                    + new String(failure, StandardCharsets.UTF_8));
        }
    }

    private static byte[] utf8(Path path) {
        return path.toString().getBytes(StandardCharsets.UTF_8);
    }

    /** Decodes the image to a PNG file and returns null, or else why it could not. */
    private static native byte[] heifToPng(byte[] image, byte[] png);
}
