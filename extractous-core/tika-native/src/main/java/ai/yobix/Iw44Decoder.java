package ai.yobix;

import org.apache.tika.exception.TikaException;

import java.util.List;

/**
 * Decodes IW44, DjVu's wavelet coding of continuous-tone images (DjVu v3 specification,
 * Appendix 1): the BG44 background and FG44 foreground of a page, and the BM44 and PM44 chunks of
 * IW44 images. The chunks of one layer refine the same wavelet coefficients slice by slice; the
 * image is the inverse transform of the coefficients they leave. Only luminance is reconstructed,
 * the grey the page is read in; chrominance is decoded, since its decisions share the stream,
 * but not transformed.
 */
final class Iw44Decoder {

    private static final int BANDS = 10;
    private static final int BLOCK = 32;
    private static final int[] BAND_FIRST_BUCKET = {0, 1, 2, 3, 4, 8, 12, 16, 32, 48};
    private static final int[] BAND_BUCKETS = {1, 1, 1, 1, 4, 4, 4, 16, 16, 16};
    private static final int[] INITIAL_STEPS = {
            0x004000, 0x008000, 0x008000, 0x010000, 0x010000, 0x010000, 0x020000, 0x020000,
            0x020000, 0x040000, 0x040000, 0x040000, 0x080000, 0x040000, 0x040000, 0x080000};
    private static final int MAX_SLICE_STEP = 0x8000;
    private static final int[] ZIGZAG_ROW = new int[1024];
    private static final int[] ZIGZAG_COLUMN = new int[1024];

    static {
        for (int i = 0; i < 1024; i++) {
            ZIGZAG_ROW[i] = bit(i, 1) << 4 | bit(i, 3) << 3 | bit(i, 5) << 2 | bit(i, 7) << 1
                    | bit(i, 9);
            ZIGZAG_COLUMN[i] = bit(i, 0) << 4 | bit(i, 2) << 3 | bit(i, 4) << 2 | bit(i, 6) << 1
                    | bit(i, 8);
        }
    }

    private static int bit(int value, int index) {
        return (value >> index) & 1;
    }

    private static final int ZERO = 1;
    private static final int ACTIVE = 2;
    private static final int NEW = 4;
    private static final int UNKNOWN = 8;

    /** A grey image, its rows top first, one byte per pixel from black (0) to white (255). */
    record GreyImage(int width, int height, byte[] pixels) {
    }

    /** The coefficients of one colour component and the state of their decoding. */
    private static final class Component {
        private final int blocksWide;
        private final int blocks;
        private final short[][] coefficients;
        private final int[] lowSteps = new int[16];
        private final int[] highSteps = new int[BANDS];
        private final int[] contexts = new int[1 + 80 + 16 + 1];
        private final int[] coefficientStates = new int[256];
        private final int[] bucketStates = new int[16];
        private int band;
        private boolean exhausted;

        Component(int width, int height) {
            blocksWide = (width + BLOCK - 1) / BLOCK;
            blocks = blocksWide * ((height + BLOCK - 1) / BLOCK);
            coefficients = new short[blocks][];
            for (int i = 0; i < 4; i++) {
                lowSteps[i] = INITIAL_STEPS[i];
            }
            for (int i = 4; i < 16; i++) {
                lowSteps[i] = INITIAL_STEPS[4 + (i - 4) / 4];
            }
            for (int b = 1; b < BANDS; b++) {
                highSteps[b] = INITIAL_STEPS[6 + b];
            }
        }

        private static int bucketContext(int band, int index) {
            return 1 + band * 8 + index;
        }

        private static int activationContext(int index) {
            return 81 + index;
        }

        private static final int ROOT = 0;
        private static final int MANTISSA = 97;

        /** Decodes one slice of this component: one band of every block (10.4, 10.5). */
        void slice(ZpDecoder zp) {
            if (exhausted) {
                return;
            }
            if (!nullSlice()) {
                for (int block = 0; block < blocks; block++) {
                    blockBand(zp, block);
                }
            }
            highSteps[band] >>= 1;
            if (band == 0) {
                for (int i = 0; i < 16; i++) {
                    lowSteps[i] >>= 1;
                }
            }
            if (++band == BANDS) {
                band = 0;
                if (highSteps[BANDS - 1] == 0) {
                    exhausted = true;
                }
            }
        }

        /** Whether no coefficient of the band can change at its present steps (10.5.1). */
        private boolean nullSlice() {
            if (band != 0) {
                return highSteps[band] <= 0 || highSteps[band] >= MAX_SLICE_STEP;
            }
            boolean empty = true;
            for (int i = 0; i < 16; i++) {
                final boolean live = lowSteps[i] > 0 && lowSteps[i] < MAX_SLICE_STEP;
                coefficientStates[i] = live ? UNKNOWN : ZERO;
                empty &= !live;
            }
            return empty;
        }

        private void blockBand(ZpDecoder zp, int block) {
            final int first = BAND_FIRST_BUCKET[band];
            final int buckets = BAND_BUCKETS[band];
            short[] data = coefficients[block];
            int blockState = 0;
            for (int bucket = 0; bucket < buckets; bucket++) {
                int bucketState = 0;
                for (int i = 0; i < 16; i++) {
                    final int index = bucket * 16 + i;
                    int state;
                    if (band == 0 && coefficientStates[index] == ZERO) {
                        state = ZERO;
                    } else {
                        state = data != null && data[(first + bucket) * 16 + i] != 0
                                ? ACTIVE : UNKNOWN;
                    }
                    coefficientStates[index] = state;
                    bucketState |= state;
                }
                bucketStates[bucket] = bucketState;
                blockState |= bucketState;
            }
            if (buckets < 16 || (blockState & ACTIVE) != 0) {
                blockState |= NEW;
            } else if ((blockState & UNKNOWN) != 0 && zp.decode(contexts, ROOT) == 1) {
                blockState |= NEW;
            }
            if ((blockState & NEW) != 0) {
                for (int bucket = 0; bucket < buckets; bucket++) {
                    if ((bucketStates[bucket] & UNKNOWN) == 0) {
                        continue;
                    }
                    int context = 0;
                    if (band > 0 && data != null) {
                        final int parent = (first + bucket) << 2;
                        for (int k = 0; k < 4 && context < 3; k++) {
                            if (data[parent + k] != 0) {
                                context++;
                            }
                        }
                    }
                    if ((blockState & ACTIVE) != 0) {
                        context |= 4;
                    }
                    if (zp.decode(contexts, bucketContext(band, context)) == 1) {
                        bucketStates[bucket] |= NEW;
                    }
                }
                for (int bucket = 0; bucket < buckets; bucket++) {
                    if ((bucketStates[bucket] & NEW) == 0) {
                        continue;
                    }
                    int potential = 0;
                    for (int i = 0; i < 16; i++) {
                        if ((coefficientStates[bucket * 16 + i] & UNKNOWN) != 0) {
                            potential++;
                        }
                    }
                    if (data == null) {
                        data = new short[1024];
                        coefficients[block] = data;
                    }
                    for (int i = 0; i < 16; i++) {
                        final int index = bucket * 16 + i;
                        if ((coefficientStates[index] & UNKNOWN) == 0) {
                            continue;
                        }
                        int context = Math.min(potential, 7);
                        if ((bucketStates[bucket] & ACTIVE) != 0) {
                            context |= 8;
                        }
                        if (zp.decode(contexts, activationContext(context)) == 1) {
                            coefficientStates[index] |= NEW;
                            final int step = step(bucket, i);
                            final int half = step >> 1;
                            final int value = step + half - (half >> 2);
                            data[(first + bucket) * 16 + i] =
                                    (short) (zp.decodeIw44PassThrough() == 1 ? -value : value);
                            potential = 0;
                        } else if (potential > 0) {
                            potential--;
                        }
                    }
                }
            }
            if ((blockState & ACTIVE) != 0) {
                for (int bucket = 0; bucket < buckets; bucket++) {
                    if ((bucketStates[bucket] & ACTIVE) == 0) {
                        continue;
                    }
                    for (int i = 0; i < 16; i++) {
                        if ((coefficientStates[bucket * 16 + i] & ACTIVE) == 0) {
                            continue;
                        }
                        final int position = (first + bucket) * 16 + i;
                        final int step = step(bucket, i);
                        int magnitude = Math.abs(data[position]);
                        final boolean increase;
                        if (magnitude <= 3 * step) {
                            magnitude += step >> 2;
                            increase = zp.decode(contexts, MANTISSA) == 1;
                        } else {
                            increase = zp.decodeIw44PassThrough() == 1;
                        }
                        magnitude += increase ? step >> 1 : -step + (step >> 1);
                        data[position] = (short) (data[position] > 0 ? magnitude : -magnitude);
                    }
                }
            }
        }

        private int step(int bucket, int i) {
            return band == 0 ? lowSteps[bucket * 16 + i] : highSteps[band];
        }

        /**
         * The component's values, rows bottom first, as signed fixed point with six fractional
         * bits: the coefficients put in place (10.6.3) and inversely transformed (10.6.4).
         */
        int[] reconstruct(int width, int height) {
            final int[] values = new int[width * height];
            for (int block = 0; block < blocks; block++) {
                final short[] data = coefficients[block];
                if (data == null) {
                    continue;
                }
                final int x0 = (block % blocksWide) * BLOCK;
                final int y0 = (block / blocksWide) * BLOCK;
                for (int i = 0; i < 1024; i++) {
                    final int x = x0 + ZIGZAG_COLUMN[i];
                    final int y = y0 + ZIGZAG_ROW[i];
                    if (x < width && y < height) {
                        values[y * width + x] = data[i];
                    }
                }
            }
            for (int scale = 16; scale >= 1; scale >>= 1) {
                for (int x = 0; x < width; x += scale) {
                    inverse(values, x, width * scale, (height - 1) / scale);
                }
                for (int y = 0; y < height; y += scale) {
                    inverse(values, y * width, scale, (width - 1) / scale);
                }
            }
            return values;
        }
    }

    /**
     * The inverse of the interpolating (4, 4) wavelet along one row or column of the values at a
     * scale: `last + 1` values from `start`, `stride` apart. Lifting restores the even ones,
     * prediction the odd ones from them (10.6.4).
     */
    private static void inverse(int[] values, int start, int stride, int last) {
        for (int k = 0; k <= last; k += 2) {
            final int near = at(values, start, stride, last, k - 1)
                    + at(values, start, stride, last, k + 1);
            final int far = at(values, start, stride, last, k - 3)
                    + at(values, start, stride, last, k + 3);
            values[start + k * stride] -= (9 * near - far + 16) >> 5;
        }
        for (int k = 1; k <= last; k += 2) {
            final int previous = values[start + (k - 1) * stride];
            final int prediction;
            if (k - 3 >= 0 && k + 3 <= last) {
                final int near = previous + values[start + (k + 1) * stride];
                final int far = values[start + (k - 3) * stride] + values[start + (k + 3) * stride];
                prediction = (9 * near - far + 8) >> 4;
            } else if (k + 1 <= last) {
                prediction = (previous + values[start + (k + 1) * stride] + 1) >> 1;
            } else {
                prediction = previous;
            }
            values[start + k * stride] += prediction;
        }
    }

    private static int at(int[] values, int start, int stride, int last, int k) {
        return k < 0 || k > last ? 0 : values[start + k * stride];
    }

    private Component luminance;
    private Component[] chrominance;
    private int width;
    private int height;
    private boolean grey;
    private int chrominanceDelay;
    private int nextSerial;

    /** The grey image a layer's chunks code, in their order. */
    static GreyImage decode(List<byte[]> chunks) throws TikaException {
        if (chunks.isEmpty()) {
            throw new TikaException("An IW44 layer has no chunks");
        }
        final Iw44Decoder decoder = new Iw44Decoder();
        for (final byte[] chunk : chunks) {
            decoder.chunk(chunk);
        }
        return decoder.image();
    }

    private void chunk(byte[] chunk) throws TikaException {
        if (chunk.length < 2) {
            throw new TikaException("An IW44 chunk ends inside its header");
        }
        final int serial = chunk[0] & 0xff;
        final int slices = chunk[1] & 0xff;
        if (serial != nextSerial) {
            throw new TikaException("IW44 chunk " + serial + " follows chunk " + (nextSerial - 1));
        }
        nextSerial++;
        int offset = 2;
        if (serial == 0) {
            if (chunk.length < 8) {
                throw new TikaException("An IW44 chunk ends inside its header");
            }
            final int major = chunk[2] & 0x7f;
            final int minor = chunk[3] & 0xff;
            if (major != 1 || minor > 2) {
                throw new TikaException("IW44 version " + major + "." + minor
                        + " is not supported");
            }
            grey = (chunk[2] & 0x80) != 0;
            width = (chunk[4] & 0xff) << 8 | (chunk[5] & 0xff);
            height = (chunk[6] & 0xff) << 8 | (chunk[7] & 0xff);
            offset = 8;
            if (minor >= 2) {
                if (chunk.length < 9) {
                    throw new TikaException("An IW44 chunk ends inside its header");
                }
                chrominanceDelay = chunk[8] & 0x7f;
                offset = 9;
            }
            if ((long) width * height > Jb2Decoder.MAX_PIXELS) {
                throw new TikaException("An IW44 image of " + width + "×" + height
                        + " pixels exceeds " + Jb2Decoder.MAX_PIXELS);
            }
            luminance = new Component(width, height);
            chrominance = grey ? new Component[0]
                    : new Component[]{new Component(width, height), new Component(width, height)};
        }
        final ZpDecoder zp = new ZpDecoder(chunk, offset, chunk.length - offset);
        for (int slice = 0; slice < slices; slice++) {
            luminance.slice(zp);
            if (chrominanceDelay > 0) {
                chrominanceDelay--;
            } else {
                for (final Component component : chrominance) {
                    component.slice(zp);
                }
            }
        }
    }

    private GreyImage image() {
        final int[] values = luminance.reconstruct(width, height);
        final byte[] pixels = new byte[width * height];
        for (int y = 0; y < height; y++) {
            for (int x = 0; x < width; x++) {
                final int value = Math.max(-128, Math.min(127,
                        (values[y * width + x] + 32) >> 6));
                pixels[(height - 1 - y) * width + x] = (byte) (grey ? 127 - value : value + 128);
            }
        }
        return new GreyImage(width, height, pixels);
    }
}
