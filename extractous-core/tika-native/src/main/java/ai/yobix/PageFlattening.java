package ai.yobix;

import java.awt.image.BufferedImage;
import java.awt.image.DataBufferByte;

/**
 * A page photographed at an angle, made flat for OCR: the page is found as the largest bright
 * region against a darker background, its corners where that region reaches furthest along the
 * diagonals, and the quadrilateral they span is mapped back to a rectangle. An image the page
 * fills, as a scan does, one without a bright region large enough to be a page, one whose page
 * is already an upright rectangle, such as a screenshot's white panel, and one whose bright
 * region fades into the dark rather than ending at a sharp edge, such as the lit middle of a
 * page in shadow, are left as they are.
 */
final class PageFlattening {

    private static final int ANALYSIS_SIZE = 800;
    private static final double MAX_PAGE_FILL = 0.85;
    private static final double MIN_PAGE_AREA = 0.2;
    private static final double MIN_CORNER_OFFSET = 0.02;
    private static final int WHITE = 255;
    private static final int EDGE_SAMPLES = 24;
    private static final double EDGE_OFFSET = 0.015;
    private static final int MIN_EDGE_CONTRAST = 64;

    private PageFlattening() {
    }

    /** The page made flat, or null when the image has no page to flatten. */
    static BufferedImage flatten(BufferedImage image) {
        final double scale = Math.min(1.0,
                (double) ANALYSIS_SIZE / Math.max(image.getWidth(), image.getHeight()));
        final int width = Math.max(1, (int) Math.round(image.getWidth() * scale));
        final int height = Math.max(1, (int) Math.round(image.getHeight() * scale));
        final int[] gray = OcrImages.gray(image, width, height).getRaster()
                .getPixels(0, 0, width, height, (int[]) null);
        final int threshold = OcrImages.otsuThreshold(gray);
        final boolean[] bright = new boolean[gray.length];
        int brightCount = 0;
        for (int i = 0; i < gray.length; i++) {
            bright[i] = gray[i] >= threshold;
            if (bright[i]) {
                brightCount++;
            }
        }
        if (brightCount > MAX_PAGE_FILL * gray.length) {
            return null;
        }
        final double[][] corners = largestRegionCorners(bright, width, height);
        if (corners == null || area(corners) < MIN_PAGE_AREA * gray.length
                || !convex(corners) || upright(corners, width, height)
                || !sharpEdges(corners, gray, width, height)) {
            return null;
        }
        for (final double[] corner : corners) {
            corner[0] /= scale;
            corner[1] /= scale;
        }
        return warp(image, corners);
    }

    /**
     * The corners of the largest 4-connected bright region, top left, top right, bottom right
     * and bottom left, or null when it is smaller than a page could be.
     */
    private static double[][] largestRegionCorners(boolean[] bright, int width, int height) {
        final int[] region = new int[bright.length];
        final int[] queue = new int[bright.length];
        int largest = 0;
        int largestSize = 0;
        int label = 0;
        for (int start = 0; start < bright.length; start++) {
            if (!bright[start] || region[start] != 0) {
                continue;
            }
            label++;
            int head = 0;
            int tail = 0;
            queue[tail++] = start;
            region[start] = label;
            while (head < tail) {
                final int pixel = queue[head++];
                final int x = pixel % width;
                final int y = pixel / width;
                if (x > 0) {
                    tail = visit(bright, region, queue, tail, pixel - 1, label);
                }
                if (x < width - 1) {
                    tail = visit(bright, region, queue, tail, pixel + 1, label);
                }
                if (y > 0) {
                    tail = visit(bright, region, queue, tail, pixel - width, label);
                }
                if (y < height - 1) {
                    tail = visit(bright, region, queue, tail, pixel + width, label);
                }
            }
            if (tail > largestSize) {
                largestSize = tail;
                largest = label;
            }
        }
        if (largestSize < MIN_PAGE_AREA * bright.length) {
            return null;
        }
        final double[][] corners = new double[4][];
        final double[] best = {Double.MAX_VALUE, -Double.MAX_VALUE, -Double.MAX_VALUE,
                Double.MAX_VALUE};
        for (int pixel = 0; pixel < region.length; pixel++) {
            if (region[pixel] != largest) {
                continue;
            }
            final double x = pixel % width;
            final double y = pixel / width;
            final double[] scores = {x + y, x - y, x + y, x - y};
            for (int corner = 0; corner < 4; corner++) {
                final boolean better = corner == 0 || corner == 3
                        ? scores[corner] < best[corner] : scores[corner] > best[corner];
                if (better) {
                    best[corner] = scores[corner];
                    corners[corner] = new double[] {x, y};
                }
            }
        }
        return new double[][] {corners[0], corners[1], corners[2], corners[3]};
    }

    /** Adds a bright pixel not yet in a region to the region being grown; the queue's new end. */
    private static int visit(boolean[] bright, int[] region, int[] queue, int tail, int pixel,
                             int label) {
        if (!bright[pixel] || region[pixel] != 0) {
            return tail;
        }
        region[pixel] = label;
        queue[tail] = pixel;
        return tail + 1;
    }

    private static double area(double[][] corners) {
        double twice = 0;
        for (int i = 0; i < 4; i++) {
            final double[] a = corners[i];
            final double[] b = corners[(i + 1) % 4];
            twice += a[0] * b[1] - b[0] * a[1];
        }
        return Math.abs(twice) / 2;
    }

    private static boolean convex(double[][] corners) {
        int sign = 0;
        for (int i = 0; i < 4; i++) {
            final double[] a = corners[i];
            final double[] b = corners[(i + 1) % 4];
            final double[] c = corners[(i + 2) % 4];
            final double cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
            final int turn = (int) Math.signum(cross);
            if (turn == 0 || sign != 0 && turn != sign) {
                return false;
            }
            sign = turn;
        }
        return true;
    }

    /** Whether the corners already make an upright rectangle, so that there is nothing to do. */
    private static boolean upright(double[][] corners, int width, int height) {
        final double tolerance = MIN_CORNER_OFFSET * Math.max(width, height);
        return Math.abs(corners[0][1] - corners[1][1]) <= tolerance
                && Math.abs(corners[3][1] - corners[2][1]) <= tolerance
                && Math.abs(corners[0][0] - corners[3][0]) <= tolerance
                && Math.abs(corners[1][0] - corners[2][0]) <= tolerance;
    }

    /**
     * Whether every side of the quadrilateral is a sharp edge: just inside it, the image is
     * markedly brighter than just outside it.
     */
    private static boolean sharpEdges(double[][] corners, int[] gray, int width, int height) {
        double centerX = 0;
        double centerY = 0;
        for (final double[] corner : corners) {
            centerX += corner[0] / 4;
            centerY += corner[1] / 4;
        }
        final double offset = EDGE_OFFSET * Math.max(width, height);
        for (int side = 0; side < 4; side++) {
            final double[] from = corners[side];
            final double[] to = corners[(side + 1) % 4];
            final double length = distance(from, to);
            double normalX = -(to[1] - from[1]) / length;
            double normalY = (to[0] - from[0]) / length;
            final double midX = (from[0] + to[0]) / 2;
            final double midY = (from[1] + to[1]) / 2;
            if (normalX * (centerX - midX) + normalY * (centerY - midY) < 0) {
                normalX = -normalX;
                normalY = -normalY;
            }
            long contrast = 0;
            for (int sample = 1; sample <= EDGE_SAMPLES; sample++) {
                final double along = (double) sample / (EDGE_SAMPLES + 1);
                final double x = from[0] + (to[0] - from[0]) * along;
                final double y = from[1] + (to[1] - from[1]) * along;
                final int inside = level(gray, width, height, x + normalX * offset,
                        y + normalY * offset);
                final int outside = level(gray, width, height, x - normalX * offset,
                        y - normalY * offset);
                if (inside < 0 || outside < 0) {
                    return false;
                }
                contrast += inside - outside;
            }
            if (contrast < (long) MIN_EDGE_CONTRAST * EDGE_SAMPLES) {
                return false;
            }
        }
        return true;
    }

    /** The gray level at a point, or -1 outside the image. */
    private static int level(int[] gray, int width, int height, double x, double y) {
        final int column = (int) Math.round(x);
        final int row = (int) Math.round(y);
        if (column < 0 || row < 0 || column >= width || row >= height) {
            return -1;
        }
        return gray[row * width + column];
    }

    /** The quadrilateral of `image` within `corners`, mapped onto an upright rectangle. */
    private static BufferedImage warp(BufferedImage image, double[][] corners) {
        final int outWidth = (int) Math.round(Math.max(distance(corners[0], corners[1]),
                distance(corners[3], corners[2])));
        final int outHeight = (int) Math.round(Math.max(distance(corners[0], corners[3]),
                distance(corners[1], corners[2])));
        final int width = image.getWidth();
        final int height = image.getHeight();
        final byte[] source = ((DataBufferByte) OcrImages.gray(image, width, height).getRaster()
                .getDataBuffer()).getData();
        final BufferedImage flat = new BufferedImage(outWidth, outHeight,
                BufferedImage.TYPE_BYTE_GRAY);
        final byte[] target = ((DataBufferByte) flat.getRaster().getDataBuffer()).getData();
        final double[] mapping = squareToQuad(corners);
        for (int v = 0; v < outHeight; v++) {
            final double t = (v + 0.5) / outHeight;
            for (int u = 0; u < outWidth; u++) {
                final double s = (u + 0.5) / outWidth;
                final double w = mapping[6] * s + mapping[7] * t + 1;
                final double x = (mapping[0] * s + mapping[1] * t + mapping[2]) / w - 0.5;
                final double y = (mapping[3] * s + mapping[4] * t + mapping[5]) / w - 0.5;
                target[v * outWidth + u] = (byte) sample(source, width, height, x, y);
            }
        }
        return flat;
    }

    private static double distance(double[] a, double[] b) {
        return Math.hypot(a[0] - b[0], a[1] - b[1]);
    }

    /**
     * The projective mapping of the unit square onto the quadrilateral (Heckbert, "Fundamentals
     * of Texture Mapping and Image Warping", 1989): {a, b, c, d, e, f, g, h} with
     * x = (a s + b t + c) / (g s + h t + 1) and y = (d s + e t + f) / (g s + h t + 1).
     */
    private static double[] squareToQuad(double[][] corners) {
        final double x0 = corners[0][0];
        final double y0 = corners[0][1];
        final double x1 = corners[1][0];
        final double y1 = corners[1][1];
        final double x2 = corners[2][0];
        final double y2 = corners[2][1];
        final double x3 = corners[3][0];
        final double y3 = corners[3][1];
        final double dx1 = x1 - x2;
        final double dx2 = x3 - x2;
        final double dx3 = x0 - x1 + x2 - x3;
        final double dy1 = y1 - y2;
        final double dy2 = y3 - y2;
        final double dy3 = y0 - y1 + y2 - y3;
        final double determinant = dx1 * dy2 - dx2 * dy1;
        final double g = (dx3 * dy2 - dx2 * dy3) / determinant;
        final double h = (dx1 * dy3 - dx3 * dy1) / determinant;
        return new double[] {x1 - x0 + g * x1, x3 - x0 + h * x3, x0,
                y1 - y0 + g * y1, y3 - y0 + h * y3, y0, g, h};
    }

    /** The gray level at a point between pixels, white outside the image. */
    private static int sample(byte[] pixels, int width, int height, double x, double y) {
        final int left = (int) Math.floor(x);
        final int top = (int) Math.floor(y);
        if (left < 0 || top < 0 || left + 1 >= width || top + 1 >= height) {
            return WHITE;
        }
        final double fx = x - left;
        final double fy = y - top;
        final int topLeft = pixels[top * width + left] & 0xff;
        final int topRight = pixels[top * width + left + 1] & 0xff;
        final int bottomLeft = pixels[(top + 1) * width + left] & 0xff;
        final int bottomRight = pixels[(top + 1) * width + left + 1] & 0xff;
        final double upper = topLeft + (topRight - topLeft) * fx;
        final double lower = bottomLeft + (bottomRight - bottomLeft) * fx;
        return (int) Math.round(upper + (lower - upper) * fy);
    }
}
