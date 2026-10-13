package ai.yobix;

import org.apache.pdfbox.text.TextPosition;

import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
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
 * direction, or more than {@link #MAX_BLOCKS} blocks, keeps its content stream order.
 */
public final class ReadingOrder {

    private static final float SAME_LINE = 0.5f;
    private static final float NEXT_LINE = 3f;
    private static final int MAX_BLOCKS = 300;
    private static final Comparator<Block> STREAM_ORDER =
            Comparator.comparingInt(block -> block.order);

    private ReadingOrder() {
    }

    /** Reorders the glyphs of a page, given in content stream order, into reading order. */
    public static void arrange(List<TextPosition> glyphs) {
        if (glyphs.size() < 2 || hasMixedDirections(glyphs)) {
            return;
        }
        final List<Block> blocks = blocks(glyphs);
        if (blocks.size() < 2 || blocks.size() > MAX_BLOCKS) {
            return;
        }
        final boolean rightToLeft = isRightToLeft(glyphs);
        glyphs.clear();
        for (final List<Block> band : bands(blocks)) {
            for (final Block block : order(band, rightToLeft)) {
                glyphs.addAll(block.glyphs);
            }
        }
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

        private static float em(TextPosition glyph) {
            return Math.max(glyph.getFontSizeInPt(), glyph.getHeightDir());
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
