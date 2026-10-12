package ai.yobix;

import java.awt.Color;
import java.awt.Graphics2D;
import java.awt.RenderingHints;
import java.awt.geom.AffineTransform;
import java.awt.image.BufferedImage;

/**
 * Corrections that make an image readable for OCR: the orientation recorded in its EXIF data, and
 * the small rotation of a page scanned or photographed slightly askew, which breaks tesseract's
 * layout analysis already at a few degrees.
 */
final class OcrImages {

    private static final int ANALYSIS_SIZE = 1200;
    private static final double MAX_SKEW_DEGREES = 10;
    private static final double COARSE_STEP_DEGREES = 0.25;
    private static final double FINE_STEP_DEGREES = 0.05;
    private static final double MIN_CORRECTION_DEGREES = 0.3;
    private static final double MIN_SHARPNESS_GAIN = 1.05;

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
        final BufferedImage reduced = new BufferedImage(width, height, BufferedImage.TYPE_BYTE_GRAY);
        final Graphics2D graphics = reduced.createGraphics();
        try {
            graphics.setRenderingHint(RenderingHints.KEY_INTERPOLATION,
                    RenderingHints.VALUE_INTERPOLATION_BILINEAR);
            graphics.drawImage(image, 0, 0, width, height, null);
        } finally {
            graphics.dispose();
        }
        final int[] gray = reduced.getRaster().getPixels(0, 0, width, height, (int[]) null);
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

    private static int otsuThreshold(int[] gray) {
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
