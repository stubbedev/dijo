package ai.yobix;

import java.awt.Color;
import java.awt.Graphics2D;
import java.awt.RenderingHints;
import java.awt.geom.AffineTransform;
import java.awt.image.BufferedImage;
import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/**
 * Corrections that make an image readable for OCR: the orientation recorded in its EXIF data, the
 * small rotation of a page scanned or photographed slightly askew, which breaks tesseract's
 * layout analysis already at a few degrees, text too small for tesseract, as in screenshots and
 * low-resolution scans, and lighting too uneven for a single binarisation threshold.
 */
final class OcrImages {

    private static final int ANALYSIS_SIZE = 1200;
    private static final double MAX_SKEW_DEGREES = 10;
    private static final double COARSE_STEP_DEGREES = 0.25;
    private static final double FINE_STEP_DEGREES = 0.05;
    private static final double MIN_CORRECTION_DEGREES = 0.3;
    private static final double MIN_SHARPNESS_GAIN = 1.05;
    private static final int MIN_LINE_PIXELS = 20;
    private static final int MAX_LINE_PIXELS = 40;
    private static final int TARGET_LINE_PIXELS = 32;
    private static final int MIN_MEASURED_LINES = 3;
    private static final int MAX_LINE_SHARE_OF_HEIGHT = 10;
    private static final double MAX_UPSCALE = 4;
    private static final int MIN_INK_RUN_PIXELS = 3;
    private static final int LIGHTING_TILES = 8;
    private static final double BACKGROUND_PERCENTILE = 0.9;
    private static final int UNEVEN_BACKGROUND_LEVELS = 48;

    private OcrImages() {
    }

    /**
     * The image turned upright according to an EXIF/TIFF orientation value (1 to 8), or the image
     * itself when the orientation is absent, 1 or not a valid value.
     */
    static BufferedImage applyOrientation(BufferedImage image, String orientation) {
        if (orientation == null) {
            return image;
        }
        final int width = image.getWidth();
        final int height = image.getHeight();
        final AffineTransform transform = new AffineTransform();
        final boolean swapped;
        switch (orientation.trim()) {
            case "2" -> {
                transform.scale(-1, 1);
                transform.translate(-width, 0);
                swapped = false;
            }
            case "3" -> {
                transform.translate(width, height);
                transform.rotate(Math.PI);
                swapped = false;
            }
            case "4" -> {
                transform.scale(1, -1);
                transform.translate(0, -height);
                swapped = false;
            }
            case "5" -> {
                transform.rotate(-Math.PI / 2);
                transform.scale(-1, 1);
                swapped = true;
            }
            case "6" -> {
                transform.translate(height, 0);
                transform.rotate(Math.PI / 2);
                swapped = true;
            }
            case "7" -> {
                transform.scale(-1, 1);
                transform.translate(-height, 0);
                transform.translate(0, width);
                transform.rotate(3 * Math.PI / 2);
                swapped = true;
            }
            case "8" -> {
                transform.translate(0, width);
                transform.rotate(3 * Math.PI / 2);
                swapped = true;
            }
            default -> {
                return image;
            }
        }
        final BufferedImage turned = new BufferedImage(
                swapped ? height : width, swapped ? width : height, BufferedImage.TYPE_INT_RGB);
        final Graphics2D graphics = turned.createGraphics();
        try {
            graphics.drawImage(image, transform, null);
        } finally {
            graphics.dispose();
        }
        return turned;
    }

    /** The image turned clockwise by a number of quarter turns. */
    static BufferedImage turnClockwise(BufferedImage image, int quarterTurns) {
        return switch (Math.floorMod(quarterTurns, 4)) {
            case 1 -> applyOrientation(image, "6");
            case 2 -> applyOrientation(image, "3");
            case 3 -> applyOrientation(image, "8");
            default -> image;
        };
    }

    /**
     * The angle, in degrees, by which the lines of text in the image are rotated, or 0 when no
     * rotation clearly lines them up better. Found by projecting the dark pixels of a reduced copy
     * onto the vertical axis at candidate angles: text lines give the sharpest profile when level.
     */
    static double skewDegrees(BufferedImage image) {
        final double scale = Math.min(1.0,
                (double) ANALYSIS_SIZE / Math.max(image.getWidth(), image.getHeight()));
        final int width = Math.max(1, (int) Math.round(image.getWidth() * scale));
        final int height = Math.max(1, (int) Math.round(image.getHeight() * scale));
        final int[] gray = gray(image, width, height).getRaster()
                .getPixels(0, 0, width, height, (int[]) null);
        final int threshold = otsuThreshold(gray);
        int count = 0;
        for (final int value : gray) {
            if (value < threshold) {
                count++;
            }
        }
        if (count == 0 || count > gray.length / 2) {
            return 0;
        }
        final int[] xs = new int[count];
        final int[] ys = new int[count];
        int index = 0;
        for (int i = 0; i < gray.length; i++) {
            if (gray[i] < threshold) {
                xs[index] = i % width;
                ys[index] = i / width;
                index++;
            }
        }
        final int diagonal = (int) Math.ceil(Math.hypot(width, height));
        final double level = sharpness(xs, ys, 0, diagonal);
        double best = 0;
        double bestSharpness = level;
        for (double angle = -MAX_SKEW_DEGREES; angle <= MAX_SKEW_DEGREES;
             angle += COARSE_STEP_DEGREES) {
            final double sharpness = sharpness(xs, ys, angle, diagonal);
            if (sharpness > bestSharpness) {
                best = angle;
                bestSharpness = sharpness;
            }
        }
        final double coarse = best;
        for (double angle = coarse - COARSE_STEP_DEGREES; angle <= coarse + COARSE_STEP_DEGREES;
             angle += FINE_STEP_DEGREES) {
            final double sharpness = sharpness(xs, ys, angle, diagonal);
            if (sharpness > bestSharpness) {
                best = angle;
                bestSharpness = sharpness;
            }
        }
        if (Math.abs(best) < MIN_CORRECTION_DEGREES || bestSharpness < level * MIN_SHARPNESS_GAIN) {
            return 0;
        }
        return best;
    }

    /**
     * How much to resize the image so its text lines are about {@value #TARGET_LINE_PIXELS}
     * pixels high: enlarged when they are below {@value #MIN_LINE_PIXELS}, since tesseract reads
     * small text poorly, but no further than maxPixels allows, and, when reduce allows it,
     * reduced when they are above {@value #MAX_LINE_PIXELS}, since tesseract reads such text as
     * well and faster once it is smaller. The line height is only trusted for reducing when at
     * least {@value #MIN_MEASURED_LINES} lines were measured, each under a tenth of the image's
     * height: on an inverted page or one with a dark border, every row holds ink. 1 when the
     * image has no lines of text.
     */
    static double scaleFactor(BufferedImage image, long maxPixels, boolean reduce) {
        final int width = image.getWidth();
        final int height = image.getHeight();
        final int[] gray = gray(image, width, height).getRaster()
                .getPixels(0, 0, width, height, (int[]) null);
        final int threshold = otsuThreshold(gray);
        final List<Integer> runs = new ArrayList<>();
        int run = 0;
        for (int y = 0; y <= height; y++) {
            if (y < height && rowHasInk(gray, y, width, threshold)) {
                run++;
                continue;
            }
            if (run >= MIN_INK_RUN_PIXELS) {
                runs.add(run);
            }
            run = 0;
        }
        if (runs.isEmpty()) {
            return 1;
        }
        Collections.sort(runs);
        final int lineHeight = runs.get(runs.size() / 2);
        if (lineHeight > MAX_LINE_PIXELS) {
            final boolean measured = runs.size() >= MIN_MEASURED_LINES
                    && runs.get(runs.size() - 1) * MAX_LINE_SHARE_OF_HEIGHT < height;
            return reduce && measured ? (double) TARGET_LINE_PIXELS / lineHeight : 1;
        }
        if (lineHeight >= MIN_LINE_PIXELS) {
            return 1;
        }
        final double wanted = Math.min(MAX_UPSCALE, (double) TARGET_LINE_PIXELS / lineHeight);
        final double allowed = Math.sqrt((double) maxPixels / ((long) width * height));
        return Math.max(1, Math.min(wanted, allowed));
    }

    private static boolean rowHasInk(int[] gray, int y, int width, int threshold) {
        for (int x = 0; x < width; x++) {
            if (gray[y * width + x] < threshold) {
                return true;
            }
        }
        return false;
    }

    /**
     * Whether the image is lit unevenly, as a photographed page in shadow is: whether the
     * background brightness of its regions differs by {@value #UNEVEN_BACKGROUND_LEVELS} gray
     * levels or more. The background of a region is the brightness its lightest tenth of pixels
     * reaches.
     */
    static boolean unevenLighting(BufferedImage image) {
        final double scale = Math.min(1.0,
                (double) ANALYSIS_SIZE / Math.max(image.getWidth(), image.getHeight()));
        final int width = Math.max(LIGHTING_TILES, (int) Math.round(image.getWidth() * scale));
        final int height = Math.max(LIGHTING_TILES, (int) Math.round(image.getHeight() * scale));
        final int[] gray = gray(image, width, height).getRaster()
                .getPixels(0, 0, width, height, (int[]) null);
        int lightest = 0;
        int darkest = 255;
        for (int tileY = 0; tileY < LIGHTING_TILES; tileY++) {
            for (int tileX = 0; tileX < LIGHTING_TILES; tileX++) {
                final int[] histogram = new int[256];
                int count = 0;
                for (int y = tileY * height / LIGHTING_TILES;
                     y < (tileY + 1) * height / LIGHTING_TILES; y++) {
                    for (int x = tileX * width / LIGHTING_TILES;
                         x < (tileX + 1) * width / LIGHTING_TILES; x++) {
                        histogram[gray[y * width + x]]++;
                        count++;
                    }
                }
                final int background = percentile(histogram, count, BACKGROUND_PERCENTILE);
                lightest = Math.max(lightest, background);
                darkest = Math.min(darkest, background);
            }
        }
        return lightest - darkest >= UNEVEN_BACKGROUND_LEVELS;
    }

    private static int percentile(int[] histogram, int count, double percentile) {
        final long wanted = (long) Math.ceil(count * percentile);
        long seen = 0;
        for (int value = 0; value < histogram.length; value++) {
            seen += histogram[value];
            if (seen >= wanted) {
                return value;
            }
        }
        return histogram.length - 1;
    }

    /** The image resized by a factor, smoothly, so the letters keep their shapes. */
    static BufferedImage scale(BufferedImage image, double factor) {
        final int width = (int) Math.round(image.getWidth() * factor);
        final int height = (int) Math.round(image.getHeight() * factor);
        final BufferedImage scaled = new BufferedImage(width, height, BufferedImage.TYPE_BYTE_GRAY);
        final Graphics2D graphics = scaled.createGraphics();
        try {
            graphics.setRenderingHint(RenderingHints.KEY_INTERPOLATION,
                    RenderingHints.VALUE_INTERPOLATION_BICUBIC);
            graphics.drawImage(image, 0, 0, width, height, null);
        } finally {
            graphics.dispose();
        }
        return scaled;
    }

    static BufferedImage gray(BufferedImage image, int width, int height) {
        if (image.getType() == BufferedImage.TYPE_BYTE_GRAY && image.getWidth() == width
                && image.getHeight() == height) {
            return image;
        }
        final BufferedImage gray = new BufferedImage(width, height, BufferedImage.TYPE_BYTE_GRAY);
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

    /** The image rotated by -degrees about its centre, on a white canvas that holds all of it. */
    static BufferedImage rotate(BufferedImage image, double degrees) {
        final double radians = Math.toRadians(-degrees);
        final double sin = Math.abs(Math.sin(radians));
        final double cos = Math.abs(Math.cos(radians));
        final int width = image.getWidth();
        final int height = image.getHeight();
        final int rotatedWidth = (int) Math.ceil(width * cos + height * sin);
        final int rotatedHeight = (int) Math.ceil(height * cos + width * sin);
        final BufferedImage rotated =
                new BufferedImage(rotatedWidth, rotatedHeight, BufferedImage.TYPE_INT_RGB);
        final Graphics2D graphics = rotated.createGraphics();
        try {
            graphics.setColor(Color.WHITE);
            graphics.fillRect(0, 0, rotatedWidth, rotatedHeight);
            graphics.setRenderingHint(RenderingHints.KEY_INTERPOLATION,
                    RenderingHints.VALUE_INTERPOLATION_BICUBIC);
            graphics.translate(rotatedWidth / 2.0, rotatedHeight / 2.0);
            graphics.rotate(radians);
            graphics.translate(-width / 2.0, -height / 2.0);
            graphics.drawImage(image, 0, 0, null);
        } finally {
            graphics.dispose();
        }
        return rotated;
    }

    private static double sharpness(int[] xs, int[] ys, double degrees, int diagonal) {
        final double radians = Math.toRadians(degrees);
        final double sin = Math.sin(radians);
        final double cos = Math.cos(radians);
        final long[] rows = new long[2 * diagonal + 1];
        for (int i = 0; i < xs.length; i++) {
            final int row = (int) Math.round(ys[i] * cos - xs[i] * sin) + diagonal;
            rows[row]++;
        }
        double sum = 0;
        for (final long row : rows) {
            sum += (double) row * row;
        }
        return sum;
    }

    static int otsuThreshold(int[] gray) {
        final long[] histogram = new long[256];
        for (final int value : gray) {
            histogram[value]++;
        }
        final long total = gray.length;
        double sumAll = 0;
        for (int value = 0; value < 256; value++) {
            sumAll += (double) value * histogram[value];
        }
        double sumBackground = 0;
        long weightBackground = 0;
        double bestVariance = -1;
        int threshold = 128;
        for (int value = 0; value < 256; value++) {
            weightBackground += histogram[value];
            if (weightBackground == 0) {
                continue;
            }
            final long weightForeground = total - weightBackground;
            if (weightForeground == 0) {
                break;
            }
            sumBackground += (double) value * histogram[value];
            final double meanBackground = sumBackground / weightBackground;
            final double meanForeground = (sumAll - sumBackground) / weightForeground;
            final double variance = (double) weightBackground * weightForeground
                    * (meanBackground - meanForeground) * (meanBackground - meanForeground);
            if (variance > bestVariance) {
                bestVariance = variance;
                threshold = value + 1;
            }
        }
        return threshold;
    }
}
