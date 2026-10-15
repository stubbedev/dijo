package ai.yobix;

import java.util.Locale;

/**
 * What was done to an image before tesseract read it, and with which model, as one JSON object
 * per image or page under {@link #KEY}: the page, the model, the script orientation and script
 * detection found, the EXIF orientation applied, whether a page photographed at an angle was made
 * flat, whether light text on a dark ground was inverted, the quarter turns and deskewing that
 * made it upright, the enlargement, whether it was thresholded for uneven lighting, and whether
 * Java could decode it to prepare it at all.
 */
final class OcrReport {

    static final String KEY = "extractous:ocr";

    private final StringBuilder json = new StringBuilder("{");

    private OcrReport() {
    }

    static OcrReport of(String language) {
        return new OcrReport().string("language", language);
    }

    OcrReport page(Integer page) {
        return page == null ? this : number("page", page);
    }

    OcrReport script(String script) {
        return script == null ? this : string("script", script);
    }

    OcrReport exifOrientation(String orientation) {
        return orientation == null
                ? this : number("exif_orientation", Integer.parseInt(orientation.trim()));
    }

    OcrReport prepared(boolean decoded, boolean flattened, boolean inverted, int quarterTurns,
                       double skewDegrees, double scale, boolean thresholded) {
        return bool("prepared", decoded)
                .bool("flattened", flattened)
                .bool("inverted", inverted)
                .number("turned_degrees", quarterTurns * 90)
                .decimal("deskewed_degrees", skewDegrees)
                .decimal("scale", scale)
                .bool("thresholded", thresholded);
    }

    String json() {
        return json + "}";
    }

    private OcrReport key(String name) {
        if (json.length() > 1) {
            json.append(',');
        }
        json.append('"').append(name).append("\":");
        return this;
    }

    private OcrReport string(String name, String value) {
        key(name).json.append('"');
        for (int index = 0; index < value.length(); index++) {
            final char c = value.charAt(index);
            switch (c) {
                case '"' -> json.append("\\\"");
                case '\\' -> json.append("\\\\");
                default -> {
                    if (c < 0x20) {
                        json.append(String.format(Locale.ROOT, "\\u%04x", (int) c));
                    } else {
                        json.append(c);
                    }
                }
            }
        }
        json.append('"');
        return this;
    }

    private OcrReport number(String name, int value) {
        key(name).json.append(value);
        return this;
    }

    private OcrReport decimal(String name, double value) {
        key(name).json.append(String.format(Locale.ROOT, "%.3f", value));
        return this;
    }

    private OcrReport bool(String name, boolean value) {
        key(name).json.append(value);
        return this;
    }
}
