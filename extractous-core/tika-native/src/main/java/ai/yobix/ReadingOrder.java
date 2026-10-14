package ai.yobix;

import org.apache.pdfbox.text.TextPosition;
import org.apache.pdfbox.util.Matrix;

import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.ListIterator;
import java.util.PriorityQueue;

/**
 * Puts the glyphs of a PDF page into reading order, for pages whose content stream draws their
 * text in another order, such as the right column before the left, or the footer first. The
 * glyphs are first taken in content stream order as blocks: a block goes on while the next glyph
 * continues its line, or starts the next line below it within its width. Blocks then fall into
 * bands, which full-width gaps separate (a header, a title, the columns, a footer), read from the
 * top. Within a band, blocks are ordered as T. M. Breuel orders text lines ("High Performance
 * Document Layout Analysis", 2003): a block comes before another below it that it overlaps
 * horizontally, and before another wholly to its right (left, on a page of right-to-left text)
 * and not wholly above it, unless a third block between them vertically spans both. Blocks those
 * rules leave unordered keep their content stream order, so a page drawn in reading order is left
 * as it is, and a table drawn row by row stays one block. A page with text in more than one
 * direction, or more than {@link #MAX_BLOCKS} blocks, keeps its blocks in content stream order.
 * <p>
 * Within a block, a line drawn in runs of different fonts, as when a font lacks the spaces and
 * punctuation of a script and they are drawn in another font before or after the letters, has
 * its runs merged from the left, unless the page is written right to left. Each run keeps its
 * own order, since the marks of complex scripts can sit left of the glyph they follow.
 * <p>
 * A page written vertically, as Japanese and Chinese are, has its glyphs drawn upright one below
 * the other, in columns read from the right; PDFBox, which breaks a line wherever the next glyph
 * sits lower, wrote each glyph on a line of its own. Where the next glyph is far more often below
 * the last than beside it, the page is turned a quarter: each column becomes a line, the columns
 * lines from the top, and the page is read as any other.
 */
public final class ReadingOrder {

    private static final float SAME_LINE = 0.5f;
    private static final float NEXT_LINE = 3f;
    private static final int MAX_BLOCKS = 300;
    private static final float SAME_COLUMN = 0.3f;
    private static final float MIN_GLYPH_STEP = 0.6f;
    private static final float MAX_GLYPH_STEP = 1.6f;
    private static final int MIN_VERTICAL_STEPS = 8;
    private static final int VERTICAL_MAJORITY = 2;
    private static final Comparator<Block> STREAM_ORDER =
            Comparator.comparingInt(block -> block.order);

    private ReadingOrder() {
    }

    /** Reorders the glyphs of a page, given in content stream order, into reading order. */
    public static void arrange(List<TextPosition> glyphs) {
        if (glyphs.size() < 2 || hasMixedDirections(glyphs)) {
            return;
        }
        if (isWrittenVertically(glyphs)) {
            turnColumnsIntoLines(glyphs);
        }
        final List<Block> blocks = blocks(glyphs);
        final boolean rightToLeft = isRightToLeft(glyphs);
        final List<Block> ordered = new ArrayList<>(blocks.size());
        if (blocks.size() > MAX_BLOCKS) {
            ordered.addAll(blocks);
        } else {
            for (final List<Block> band : bands(blocks)) {
                ordered.addAll(order(band, rightToLeft));
            }
        }
        glyphs.clear();
        for (final Block block : ordered) {
            if (!rightToLeft) {
                orderLinesFromTheLeft(block.glyphs);
            }
            glyphs.addAll(block.glyphs);
        }
    }

    /**
     * Merges the font runs of each line, a run of glyphs on one baseline, from the left, unless
     * the glyphs are written right to left or in more than one direction. Lines keep their order.
     */
    public static void orderLines(List<TextPosition> glyphs) {
        if (glyphs.size() < 2 || hasMixedDirections(glyphs) || isRightToLeft(glyphs)) {
            return;
        }
        orderLinesFromTheLeft(glyphs);
    }

    private static void orderLinesFromTheLeft(List<TextPosition> glyphs) {
        int start = 0;
        for (int end = 1; end <= glyphs.size(); end++) {
            if (end == glyphs.size() || !onOneLine(glyphs.get(start), glyphs.get(end))) {
                mergeFontRuns(glyphs.subList(start, end));
                start = end;
            }
        }
    }

    /**
     * Merges the runs of glyphs in one font, each kept in its order, by taking next the glyph
     * furthest left among the runs' next glyphs, from the earlier run on a tie.
     */
    private static void mergeFontRuns(List<TextPosition> line) {
        final List<List<TextPosition>> runs = new ArrayList<>();
        for (int index = 0; index < line.size(); index++) {
            if (index == 0 || line.get(index).getFont() != line.get(index - 1).getFont()) {
                runs.add(new ArrayList<>());
            }
            runs.get(runs.size() - 1).add(line.get(index));
        }
        if (runs.size() < 2) {
            return;
        }
        final int[] next = new int[runs.size()];
        for (int index = 0; index < line.size(); index++) {
            int leftmost = -1;
            for (int run = 0; run < runs.size(); run++) {
                if (next[run] < runs.get(run).size() && (leftmost < 0
                        || runs.get(run).get(next[run]).getXDirAdj()
                        < runs.get(leftmost).get(next[leftmost]).getXDirAdj())) {
                    leftmost = run;
                }
            }
            line.set(index, runs.get(leftmost).get(next[leftmost]++));
        }
    }

    private static boolean onOneLine(TextPosition one, TextPosition other) {
        return Math.abs(other.getYDirAdj() - one.getYDirAdj())
                <= SAME_LINE * Math.max(em(one), em(other));
    }

    /** The blocks in bands from the top, each in content stream order. */
    private static List<List<Block>> bands(List<Block> blocks) {
        final List<Block> fromTop = new ArrayList<>(blocks);
        fromTop.sort(Comparator.comparingDouble(block -> block.top));
        final List<List<Block>> bands = new ArrayList<>();
        List<Block> band = null;
        float bandBottom = 0;
        for (final Block block : fromTop) {
            if (band == null || block.top >= bandBottom) {
                band = new ArrayList<>();
                bands.add(band);
                bandBottom = block.bottom;
            }
            band.add(block);
            bandBottom = Math.max(bandBottom, block.bottom);
        }
        for (final List<Block> each : bands) {
            each.sort(STREAM_ORDER);
        }
        return bands;
    }

    /**
     * Whether the glyphs, in content stream order, step down a column far more often than along
     * a line.
     */
    private static boolean isWrittenVertically(List<TextPosition> glyphs) {
        int down = 0;
        int along = 0;
        TextPosition last = null;
        for (final TextPosition glyph : glyphs) {
            if (glyph.getUnicode() == null || glyph.getUnicode().isBlank()) {
                continue;
            }
            if (last != null) {
                final float em = Math.max(em(last), em(glyph));
                final float dx = glyph.getXDirAdj() - last.getXDirAdj();
                final float dy = glyph.getYDirAdj() - last.getYDirAdj();
                if (Math.abs(dx) <= SAME_COLUMN * em && dy >= MIN_GLYPH_STEP * em
                        && dy <= MAX_GLYPH_STEP * em) {
                    down++;
                } else if (Math.abs(dy) <= SAME_COLUMN * em && dx > 0
                        && dx <= MAX_GLYPH_STEP * em) {
                    along++;
                }
            }
            last = glyph;
        }
        return down >= MIN_VERTICAL_STEPS && down > VERTICAL_MAJORITY * along;
    }

    /**
     * Replaces each glyph by one on a page turned a quarter clockwise: down a column becomes
     * along a line, and the rightmost column the top line.
     */
    private static void turnColumnsIntoLines(List<TextPosition> glyphs) {
        final ListIterator<TextPosition> each = glyphs.listIterator();
        while (each.hasNext()) {
            final TextPosition glyph = each.next();
            final Matrix matrix = glyph.getTextMatrix();
            final float x = glyph.getYDirAdj();
            final float y = glyph.getXDirAdj();
            final float width = glyph.getWidthDirAdj();
            each.set(new TextPosition(0, glyph.getPageHeight(), glyph.getPageWidth(),
                    new Matrix(matrix.getScaleX(), 0, 0, matrix.getScaleY(), x, y), x + width, y,
                    glyph.getHeightDir(), width, glyph.getWidthOfSpace(), glyph.getUnicode(),
                    glyph.getCharacterCodes(), glyph.getFont(), glyph.getFontSize(),
                    (int) glyph.getFontSizeInPt()));
        }
    }

    private static float em(TextPosition glyph) {
        return Math.max(glyph.getFontSizeInPt(), glyph.getHeightDir());
    }

    private static boolean hasMixedDirections(List<TextPosition> glyphs) {
        final float direction = glyphs.get(0).getDir();
        for (final TextPosition glyph : glyphs) {
            if (glyph.getDir() != direction) {
                return true;
            }
        }
        return false;
    }

    private static boolean isRightToLeft(List<TextPosition> glyphs) {
        int balance = 0;
        for (final TextPosition glyph : glyphs) {
            final String text = glyph.getUnicode();
            if (text == null || text.isEmpty()) {
                continue;
            }
            switch (Character.getDirectionality(text.codePointAt(0))) {
                case Character.DIRECTIONALITY_RIGHT_TO_LEFT,
                     Character.DIRECTIONALITY_RIGHT_TO_LEFT_ARABIC -> balance++;
                case Character.DIRECTIONALITY_LEFT_TO_RIGHT -> balance--;
                default -> {
                }
            }
        }
        return balance > 0;
    }

    /** A run of glyphs in content stream order, with the box they cover. */
    private static final class Block {
        final int order;
        final List<TextPosition> glyphs = new ArrayList<>();
        final List<Block> successors = new ArrayList<>();
        int predecessors;
        boolean placed;
        float left = Float.MAX_VALUE;
        float top = Float.MAX_VALUE;
        float right = -Float.MAX_VALUE;
        float bottom = -Float.MAX_VALUE;
        float baseline;
        float em;

        Block(int order) {
            this.order = order;
        }

        boolean continuedBy(TextPosition glyph) {
            final float x = glyph.getXDirAdj();
            final float y = glyph.getYDirAdj();
            final float lineEm = Math.max(em, em(glyph));
            if (Math.abs(y - baseline) <= SAME_LINE * lineEm) {
                return true;
            }
            return y > baseline && y - baseline <= NEXT_LINE * lineEm
                    && x >= left - lineEm && x <= right + lineEm;
        }

        void add(TextPosition glyph) {
            glyphs.add(glyph);
            final float x = glyph.getXDirAdj();
            final float y = glyph.getYDirAdj();
            left = Math.min(left, x);
            right = Math.max(right, x + glyph.getWidthDirAdj());
            top = Math.min(top, y - glyph.getHeightDir());
            bottom = Math.max(bottom, y);
            baseline = y;
            em = em(glyph);
        }

        boolean overlapsHorizontally(Block other) {
            return left < other.right && other.left < right;
        }

        boolean isAbove(Block other) {
            return bottom <= other.top;
        }

        boolean isBetweenVertically(Block one, Block other) {
            return top >= Math.min(one.bottom, other.bottom)
                    && bottom <= Math.max(one.top, other.top);
        }
    }

    private static List<Block> blocks(List<TextPosition> glyphs) {
        final List<Block> blocks = new ArrayList<>();
        Block block = null;
        for (final TextPosition glyph : glyphs) {
            if (block == null || !block.continuedBy(glyph)) {
                block = new Block(blocks.size());
                blocks.add(block);
            }
            block.add(glyph);
        }
        return blocks;
    }

    private static List<Block> order(List<Block> blocks, boolean rightToLeft) {
        for (final Block one : blocks) {
            for (final Block other : blocks) {
                if (one != other && precedes(one, other, blocks, rightToLeft)) {
                    one.successors.add(other);
                    other.predecessors++;
                }
            }
        }
        final PriorityQueue<Block> ready = new PriorityQueue<>(STREAM_ORDER);
        final PriorityQueue<Block> waiting = new PriorityQueue<>(STREAM_ORDER);
        for (final Block block : blocks) {
            (block.predecessors == 0 ? ready : waiting).add(block);
        }
        final List<Block> ordered = new ArrayList<>(blocks.size());
        while (ordered.size() < blocks.size()) {
            final Block next = ready.isEmpty() ? waiting.poll() : ready.poll();
            next.placed = true;
            ordered.add(next);
            for (final Block successor : next.successors) {
                if (!successor.placed && --successor.predecessors == 0) {
                    waiting.remove(successor);
                    ready.add(successor);
                }
            }
        }
        return ordered;
    }

    private static boolean precedes(Block one, Block other, List<Block> blocks,
                                    boolean rightToLeft) {
        if (one.overlapsHorizontally(other)) {
            return one.isAbove(other);
        }
        final boolean before = rightToLeft ? one.left >= other.right : one.right <= other.left;
        if (!before || other.isAbove(one)) {
            return false;
        }
        for (final Block between : blocks) {
            if (between != one && between != other && between.overlapsHorizontally(one)
                    && between.overlapsHorizontally(other)
                    && between.isBetweenVertically(one, other)) {
                return false;
            }
        }
        return true;
    }
}
