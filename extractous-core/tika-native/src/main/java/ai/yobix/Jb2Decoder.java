package ai.yobix;

import org.apache.tika.exception.TikaException;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

/**
 * Decodes JB2, DjVu's coding of bilevel images (DjVu v3 specification, Appendix 2): the mask of
 * a page an Sjbz chunk holds, and the shapes a Djbz chunk shares among the pages of a document.
 * A JB2 stream is a sequence of records, every field of them coded with the Z′ coder, that place
 * symbols on a white image: coded directly, refined from a shape in the library, or copied.
 */
final class Jb2Decoder {

    /** The most pixels an image may have, beyond which decoding fails rather than allocate. */
    static final long MAX_PIXELS = 1L << 28;

    private static final int BIG_POSITIVE = 262142;
    private static final int BIG_NEGATIVE = -262143;

    private static final int START_OF_DATA = 0;
    private static final int NEW_MARK = 1;
    private static final int NEW_MARK_LIBRARY_ONLY = 2;
    private static final int NEW_MARK_IMAGE_ONLY = 3;
    private static final int MATCHED_REFINE = 4;
    private static final int MATCHED_REFINE_LIBRARY_ONLY = 5;
    private static final int MATCHED_REFINE_IMAGE_ONLY = 6;
    private static final int MATCHED_COPY = 7;
    private static final int NON_MARK_DATA = 8;
    private static final int REQUIRED_DICT_OR_RESET = 9;
    private static final int PRESERVED_COMMENT = 10;
    private static final int END_OF_DATA = 11;

    private static final int OFFSET_TYPE = 0;
    private static final int REFINEMENT_FLAG = 1;

    /** A bilevel image, its rows top first, one byte per pixel: 1 for black, 0 for white. */
    record Bitmap(int width, int height, byte[] pixels) {

        int at(int x, int y) {
            return x < 0 || y < 0 || x >= width || y >= height ? 0 : pixels[y * width + x];
        }

        /** The bitmap without its white edge rows and columns. */
        Bitmap cropped() {
            int left = width;
            int right = -1;
            int top = height;
            int bottom = -1;
            for (int y = 0; y < height; y++) {
                for (int x = 0; x < width; x++) {
                    if (pixels[y * width + x] != 0) {
                        left = Math.min(left, x);
                        right = Math.max(right, x);
                        top = Math.min(top, y);
                        bottom = Math.max(bottom, y);
                    }
                }
            }
            if (right < 0) {
                return new Bitmap(0, 0, new byte[0]);
            }
            final int croppedWidth = right - left + 1;
            final int croppedHeight = bottom - top + 1;
            final byte[] cropped = new byte[croppedWidth * croppedHeight];
            for (int y = 0; y < croppedHeight; y++) {
                System.arraycopy(pixels, (top + y) * width + left, cropped, y * croppedWidth,
                        croppedWidth);
            }
            return new Bitmap(croppedWidth, croppedHeight, cropped);
        }
    }

    /** A symbol placed on the image, its top left corner at column `x` and row `y`. */
    record Blit(Bitmap mark, int x, int y) {
    }

    /** An image and the symbols placed on it, in the order they were placed. */
    record Mask(Bitmap image, List<Blit> blits) {
    }

    /**
     * One of the integer contexts of the multivalue extension (11.2.2): a binary decision tree
     * whose nodes, each a Z′ context, are allocated as they are first reached.
     */
    private static final class NumberContext {
        private int[] states = new int[64];
        private int[] left = new int[64];
        private int[] right = new int[64];
        private int nodes = 1;

        void reset() {
            states = new int[64];
            left = new int[64];
            right = new int[64];
            nodes = 1;
        }

        int child(int node, boolean decision) {
            final int[] children = decision ? right : left;
            if (children[node] == 0) {
                if (nodes == states.length) {
                    states = Arrays.copyOf(states, nodes * 2);
                    left = Arrays.copyOf(left, nodes * 2);
                    right = Arrays.copyOf(right, nodes * 2);
                }
                (decision ? right : left)[node] = nodes++;
                return nodes - 1;
            }
            return children[node];
        }
    }

    private final ZpDecoder zp;
    private final int[] bits = new int[2];
    private final int[] direct = new int[1024];
    private final int[] refinement = new int[2048];
    private final NumberContext recordType = new NumberContext();
    private final NumberContext imageSize = new NumberContext();
    private final NumberContext matchIndex = new NumberContext();
    private final NumberContext symbolWidth = new NumberContext();
    private final NumberContext symbolHeight = new NumberContext();
    private final NumberContext widthDifference = new NumberContext();
    private final NumberContext heightDifference = new NumberContext();
    private final NumberContext absoluteColumn = new NumberContext();
    private final NumberContext absoluteRow = new NumberContext();
    private final NumberContext sameLineColumn = new NumberContext();
    private final NumberContext sameLineRow = new NumberContext();
    private final NumberContext newLineColumn = new NumberContext();
    private final NumberContext newLineRow = new NumberContext();
    private final NumberContext commentLength = new NumberContext();
    private final NumberContext commentOctet = new NumberContext();
    private final NumberContext dictionarySize = new NumberContext();
    private final NumberContext[] numbers = {
            recordType, imageSize, matchIndex, symbolWidth, symbolHeight, widthDifference,
            heightDifference, absoluteColumn, absoluteRow, sameLineColumn, sameLineRow,
            newLineColumn, newLineRow, commentLength, commentOctet, dictionarySize};
    private final List<Bitmap> library = new ArrayList<>();
    private final List<Blit> blits = new ArrayList<>();
    private final List<Bitmap> dictionary;
    private final boolean imageWanted;

    private Bitmap image;
    private int lastRowLeft;
    private int lastRowBottom;
    private int lastRight;
    private final int[] bottoms = new int[3];
    private int bottomIndex;
    private int baseline;

    private Jb2Decoder(byte[] chunk, List<Bitmap> dictionary, boolean imageWanted) {
        this.zp = new ZpDecoder(chunk);
        this.dictionary = dictionary;
        this.imageWanted = imageWanted;
    }

    /**
     * The image an Sjbz chunk codes, with the shapes of the dictionary it requires, if any, in
     * `dictionary`, and the symbols it places.
     */
    static Mask decodeImage(byte[] chunk, List<Bitmap> dictionary) throws TikaException {
        final Jb2Decoder decoder = new Jb2Decoder(chunk, dictionary, true);
        decoder.records();
        if (decoder.image == null) {
            throw new TikaException("A JB2 image ends before its start of data");
        }
        return new Mask(decoder.image, List.copyOf(decoder.blits));
    }

    /** The shapes a Djbz chunk defines, after those of the dictionary it requires, if any. */
    static List<Bitmap> decodeDictionary(byte[] chunk, List<Bitmap> dictionary)
            throws TikaException {
        final Jb2Decoder decoder = new Jb2Decoder(chunk, dictionary, false);
        decoder.records();
        return List.copyOf(decoder.library);
    }

    private void records() throws TikaException {
        boolean started = false;
        while (true) {
            final int type = number(recordType, START_OF_DATA, END_OF_DATA);
            if (!started && type != START_OF_DATA && type != REQUIRED_DICT_OR_RESET
                    && type != PRESERVED_COMMENT) {
                throw new TikaException("JB2 record " + type + " comes before the start of data");
            }
            switch (type) {
                case START_OF_DATA -> {
                    if (started) {
                        throw new TikaException("A JB2 stream starts twice");
                    }
                    start();
                    started = true;
                }
                case NEW_MARK -> {
                    final Bitmap mark = direct(symbolSize());
                    place(mark, relativeLocation(mark));
                    library.add(mark.cropped());
                }
                case NEW_MARK_LIBRARY_ONLY -> library.add(direct(symbolSize()).cropped());
                case NEW_MARK_IMAGE_ONLY -> {
                    final Bitmap mark = direct(symbolSize());
                    place(mark, relativeLocation(mark));
                }
                case MATCHED_REFINE -> {
                    final Bitmap mark = refined(match());
                    place(mark, relativeLocation(mark));
                    library.add(mark.cropped());
                }
                case MATCHED_REFINE_LIBRARY_ONLY -> library.add(refined(match()).cropped());
                case MATCHED_REFINE_IMAGE_ONLY -> {
                    final Bitmap mark = refined(match());
                    place(mark, relativeLocation(mark));
                }
                case MATCHED_COPY -> {
                    final Bitmap mark = match();
                    place(mark, relativeLocation(mark));
                }
                case NON_MARK_DATA -> {
                    final Bitmap mark = direct(symbolSize());
                    place(mark, absoluteLocation(mark));
                }
                case REQUIRED_DICT_OR_RESET -> {
                    if (started) {
                        for (final NumberContext context : numbers) {
                            context.reset();
                        }
                    } else {
                        inherit(number(dictionarySize, 0, BIG_POSITIVE));
                    }
                }
                case PRESERVED_COMMENT -> {
                    final int length = number(commentLength, 0, BIG_POSITIVE);
                    for (int i = 0; i < length; i++) {
                        number(commentOctet, 0, 255);
                    }
                }
                case END_OF_DATA -> {
                    return;
                }
                default -> throw new IllegalStateException("record type " + type);
            }
        }
    }

    private void start() throws TikaException {
        final int width = number(imageSize, 0, BIG_POSITIVE);
        final int height = number(imageSize, 0, BIG_POSITIVE);
        if (zp.decode(bits, REFINEMENT_FLAG) != 0) {
            throw new TikaException("JB2 image refinement data is not supported");
        }
        if (imageWanted) {
            image = new Bitmap(width, height, new byte[checkedPixels(width, height)]);
        }
        lastRowLeft = 0;
        lastRowBottom = height;
        lastRight = 0;
        startLine(lastRowBottom);
    }

    private void inherit(int shapes) throws TikaException {
        if (!library.isEmpty()) {
            throw new TikaException("A JB2 stream requires a dictionary after defining shapes");
        }
        if (dictionary == null || dictionary.size() != shapes) {
            throw new TikaException("A JB2 stream requires a dictionary of " + shapes
                    + " shapes, but " + (dictionary == null ? "has none"
                    : "its dictionary has " + dictionary.size()));
        }
        library.addAll(dictionary);
    }

    private static int checkedPixels(int width, int height) throws TikaException {
        final long pixels = (long) width * height;
        if (pixels > MAX_PIXELS) {
            throw new TikaException("A JB2 bitmap of " + width + "×" + height
                    + " pixels exceeds " + MAX_PIXELS);
        }
        return (int) pixels;
    }

    private Bitmap symbolSize() throws TikaException {
        final int width = number(symbolWidth, 0, BIG_POSITIVE);
        final int height = number(symbolHeight, 0, BIG_POSITIVE);
        return new Bitmap(width, height, new byte[checkedPixels(width, height)]);
    }

    private Bitmap match() throws TikaException {
        if (library.isEmpty()) {
            throw new TikaException("A JB2 record matches a shape of an empty library");
        }
        return library.get(number(matchIndex, 0, library.size() - 1));
    }

    /** Codes a bitmap's pixels with the ten-pixel template of Figure 2 (11.5.9). */
    private Bitmap direct(Bitmap mark) {
        final int width = mark.width;
        final byte[] pixels = mark.pixels;
        for (int y = 0; y < mark.height; y++) {
            for (int x = 0; x < width; x++) {
                final int context = mark.at(x - 1, y - 2) << 9 | mark.at(x, y - 2) << 8
                        | mark.at(x + 1, y - 2) << 7 | mark.at(x - 2, y - 1) << 6
                        | mark.at(x - 1, y - 1) << 5 | mark.at(x, y - 1) << 4
                        | mark.at(x + 1, y - 1) << 3 | mark.at(x + 2, y - 1) << 2
                        | mark.at(x - 2, y) << 1 | mark.at(x - 1, y);
                pixels[y * width + x] = (byte) zp.decode(direct, context);
            }
        }
        return mark;
    }

    /**
     * Codes a bitmap's pixels with the eleven-pixel template of Figure 3 (11.5.10), four of its
     * own and seven of the shape it refines, the two aligned on their centres: the left of two
     * central columns and the lower of two central rows.
     */
    private Bitmap refined(Bitmap reference) throws TikaException {
        final int width = reference.width + number(widthDifference, BIG_NEGATIVE, BIG_POSITIVE);
        final int height = reference.height + number(heightDifference, BIG_NEGATIVE, BIG_POSITIVE);
        if (width < 0 || height < 0) {
            throw new TikaException("A refined JB2 shape has a negative size");
        }
        final Bitmap mark = new Bitmap(width, height, new byte[checkedPixels(width, height)]);
        final int dx = (reference.width - 1) / 2 - (width - 1) / 2;
        final int dy = reference.height / 2 - height / 2;
        final byte[] pixels = mark.pixels;
        for (int y = 0; y < height; y++) {
            final int ry = y + dy;
            for (int x = 0; x < width; x++) {
                final int rx = x + dx;
                final int context = mark.at(x - 1, y - 1) << 10 | mark.at(x, y - 1) << 9
                        | mark.at(x + 1, y - 1) << 8 | mark.at(x - 1, y) << 7
                        | reference.at(rx, ry - 1) << 6 | reference.at(rx - 1, ry) << 5
                        | reference.at(rx, ry) << 4 | reference.at(rx + 1, ry) << 3
                        | reference.at(rx - 1, ry + 1) << 2 | reference.at(rx, ry + 1) << 1
                        | reference.at(rx + 1, ry + 1);
                pixels[y * width + x] = (byte) zp.decode(refinement, context);
            }
        }
        return mark;
    }

    /** Where a symbol goes, as the column of its left edge and the row of its bottom edge. */
    private record Location(int left, int bottom) {
    }

    /**
     * The location of a symbol relative to the first symbol of the current line, starting a new
     * line, or to the previous symbol on it, against the median bottom of the line's last three
     * symbols (11.5.8). Columns count from 1 at the left, rows from 1 at the bottom.
     */
    private Location relativeLocation(Bitmap mark) {
        if (zp.decode(bits, OFFSET_TYPE) == 1) {
            final int left = lastRowLeft + number(newLineColumn, BIG_NEGATIVE, BIG_POSITIVE);
            final int top = lastRowBottom + number(newLineRow, BIG_NEGATIVE, BIG_POSITIVE);
            final int bottom = top - mark.height + 1;
            lastRowLeft = left;
            lastRowBottom = bottom;
            lastRight = left + mark.width - 1;
            startLine(bottom);
            return new Location(left, bottom);
        }
        final int left = lastRight + number(sameLineColumn, BIG_NEGATIVE, BIG_POSITIVE);
        final int bottom = baseline + number(sameLineRow, BIG_NEGATIVE, BIG_POSITIVE);
        lastRight = left + mark.width - 1;
        bottomIndex = (bottomIndex + 1) % bottoms.length;
        bottoms[bottomIndex] = bottom;
        baseline = median(bottoms[0], bottoms[1], bottoms[2]);
        return new Location(left, bottom);
    }

    private void startLine(int bottom) {
        Arrays.fill(bottoms, bottom);
        bottomIndex = 0;
        baseline = bottom;
    }

    private static int median(int a, int b, int c) {
        return Math.max(Math.min(a, b), Math.min(Math.max(a, b), c));
    }

    /** The location of non-symbol data, from its top left corner (11.5.7). */
    private Location absoluteLocation(Bitmap mark) {
        final int left = number(absoluteColumn, 1, image.width);
        final int top = number(absoluteRow, 1, image.height);
        return new Location(left, top - mark.height + 1);
    }

    private void place(Bitmap mark, Location location) throws TikaException {
        if (!imageWanted) {
            throw new TikaException("A JB2 dictionary places a symbol on an image");
        }
        final int x0 = location.left - 1;
        final int y0 = image.height - (location.bottom + mark.height - 1);
        blits.add(new Blit(mark, x0, y0));
        for (int y = Math.max(0, -y0); y < mark.height && y0 + y < image.height; y++) {
            for (int x = Math.max(0, -x0); x < mark.width && x0 + x < image.width; x++) {
                if (mark.pixels[y * mark.width + x] != 0) {
                    image.pixels[(y0 + y) * image.width + x0 + x] = 1;
                }
            }
        }
    }

    /**
     * An integer from `low` to `high` (11.2.5): its sign, then the doubling range it falls in,
     * then its bits within that range, each decision skipped when the bounds settle it.
     */
    private int number(NumberContext context, int low, int high) {
        boolean negative = false;
        int cutoff = 0;
        int node = 0;
        int phase = 1;
        int range = -1;
        while (range != 1) {
            final boolean decision = low < cutoff && high >= cutoff
                    ? zp.decode(context.states, node) == 1 : low >= cutoff;
            node = context.child(node, decision);
            switch (phase) {
                case 1 -> {
                    negative = !decision;
                    if (negative) {
                        final int lowered = -high - 1;
                        high = -low - 1;
                        low = lowered;
                    }
                    phase = 2;
                    cutoff = 1;
                }
                case 2 -> {
                    if (decision) {
                        cutoff = 2 * cutoff + 1;
                    } else {
                        phase = 3;
                        range = (cutoff + 1) / 2;
                        cutoff = range == 1 ? 0 : cutoff - range / 2;
                    }
                }
                default -> {
                    range /= 2;
                    if (range != 1) {
                        cutoff += decision ? range / 2 : -(range / 2);
                    } else if (!decision) {
                        cutoff--;
                    }
                }
            }
        }
        return negative ? -cutoff - 1 : cutoff;
    }
}
