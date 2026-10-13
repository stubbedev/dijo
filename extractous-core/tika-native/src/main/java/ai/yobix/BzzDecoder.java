package ai.yobix;

import org.apache.tika.exception.TikaException;

import java.io.ByteArrayOutputStream;

/**
 * Decompresses BZZ, the general-purpose compression of DjVu files: blocks of a Burrows-Wheeler
 * transform whose move-to-front ranks are coded with the Z′ adaptive binary arithmetic coder,
 * as the DjVu v3 specification describes them (Appendices 3 and 4, and the coder's state tables
 * of Table 9).
 */
final class BzzDecoder {

    private static final int MAX_BLOCK_BYTES = 4096 * 1024;
    private static final int CONTEXTS = 260;

    private static final int[] DELTA = {
            0x8000, 0x8000, 0x8000, 0x6bbd, 0x6bbd, 0x5d45, 0x5d45, 0x51b9, 0x51b9, 0x4813,
            0x4813, 0x3fd5, 0x3fd5, 0x38b1, 0x38b1, 0x3275, 0x3275, 0x2cfd, 0x2cfd, 0x2825,
            0x2825, 0x23ab, 0x23ab, 0x1f87, 0x1f87, 0x1bbb, 0x1bbb, 0x1845, 0x1845, 0x1523,
            0x1523, 0x1253, 0x1253, 0x0fcf, 0x0fcf, 0x0d95, 0x0d95, 0x0b9d, 0x0b9d, 0x09e3,
            0x09e3, 0x0861, 0x0861, 0x0711, 0x0711, 0x05f1, 0x05f1, 0x04f9, 0x04f9, 0x0425,
            0x0425, 0x0371, 0x0371, 0x02d9, 0x02d9, 0x0259, 0x0259, 0x01ed, 0x01ed, 0x0193,
            0x0193, 0x0149, 0x0149, 0x010b, 0x010b, 0x00d5, 0x00d5, 0x00a5, 0x00a5, 0x007b,
            0x007b, 0x0057, 0x0057, 0x003b, 0x003b, 0x0023, 0x0023, 0x0013, 0x0013, 0x0007,
            0x0007, 0x0001, 0x0001, 0x5695, 0x24ee, 0x8000, 0x0d30, 0x481a, 0x0481, 0x3579,
            0x017a, 0x24ef, 0x007b, 0x1978, 0x0028, 0x10ca, 0x000d, 0x0b5d, 0x0034, 0x078a,
            0x00a0, 0x050f, 0x0117, 0x0358, 0x01ea, 0x0234, 0x0144, 0x0173, 0x0234, 0x00f5,
            0x0353, 0x00a1, 0x05c5, 0x011a, 0x03cf, 0x01aa, 0x0285, 0x0286, 0x01ab, 0x03d3,
            0x011a, 0x05c5, 0x00ba, 0x08ad, 0x007a, 0x0ccc, 0x01eb, 0x1302, 0x02e6, 0x1b81,
            0x045e, 0x24ef, 0x0690, 0x2865, 0x09de, 0x3987, 0x0dc8, 0x2c99, 0x10ca, 0x3b5f,
            0x0b5d, 0x5695, 0x078a, 0x8000, 0x050f, 0x24ee, 0x0358, 0x0d30, 0x0234, 0x0481,
            0x0173, 0x017a, 0x00f5, 0x007b, 0x00a1, 0x0028, 0x011a, 0x000d, 0x01aa, 0x0034,
            0x0286, 0x00a0, 0x03d3, 0x0117, 0x05c5, 0x01ea, 0x08ad, 0x0144, 0x0ccc, 0x0234,
            0x1302, 0x0353, 0x1b81, 0x05c5, 0x24ef, 0x03cf, 0x2b74, 0x0285, 0x201d, 0x01ab,
            0x1715, 0x011a, 0x0fb7, 0x00ba, 0x0a67, 0x01eb, 0x06e7, 0x02e6, 0x0496, 0x045e,
            0x030d, 0x0690, 0x0206, 0x09de, 0x0155, 0x0dc8, 0x00e1, 0x2b74, 0x0094, 0x201d,
            0x0188, 0x1715, 0x0252, 0x0fb7, 0x0383, 0x0a67, 0x0547, 0x06e7, 0x07e2, 0x0496,
            0x0bc0, 0x030d, 0x1178, 0x0206, 0x19da, 0x0155, 0x24ef, 0x00e1, 0x320e, 0x0094,
            0x432a, 0x0188, 0x447d, 0x0252, 0x5ece, 0x0383, 0x8000, 0x0547, 0x481a, 0x07e2,
            0x3579, 0x0bc0, 0x24ef, 0x1178, 0x1978, 0x19da, 0x2865, 0x24ef, 0x3987, 0x320e,
            0x2c99, 0x432a, 0x3b5f, 0x447d, 0x5695, 0x5ece, 0x8000, 0x8000, 0x5695, 0x481a,
            0x481a
    };
    private static final int[] THRESHOLD = {
            0x0000, 0x0000, 0x0000, 0x10a5, 0x10a5, 0x1f28, 0x1f28, 0x2bd3, 0x2bd3, 0x36e3,
            0x36e3, 0x408c, 0x408c, 0x48fd, 0x48fd, 0x505d, 0x505d, 0x56d0, 0x56d0, 0x5c71,
            0x5c71, 0x615b, 0x615b, 0x65a5, 0x65a5, 0x6962, 0x6962, 0x6ca2, 0x6ca2, 0x6f74,
            0x6f74, 0x71e6, 0x71e6, 0x7404, 0x7404, 0x75d6, 0x75d6, 0x7768, 0x7768, 0x78c2,
            0x78c2, 0x79ea, 0x79ea, 0x7ae7, 0x7ae7, 0x7bbe, 0x7bbe, 0x7c75, 0x7c75, 0x7d0f,
            0x7d0f, 0x7d91, 0x7d91, 0x7dfe, 0x7dfe, 0x7e5a, 0x7e5a, 0x7ea6, 0x7ea6, 0x7ee6,
            0x7ee6, 0x7f1a, 0x7f1a, 0x7f45, 0x7f45, 0x7f6b, 0x7f6b, 0x7f8d, 0x7f8d, 0x7faa,
            0x7faa, 0x7fc3, 0x7fc3, 0x7fd7, 0x7fd7, 0x7fe7, 0x7fe7, 0x7ff2, 0x7ff2, 0x7ffa,
            0x7ffa, 0x7fff, 0x7fff, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000, 0x0000,
            0x0000
    };
    private static final int[] AFTER_MPS = {
            84, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17,
            18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33,
            34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49,
            50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65,
            66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81,
            82, 81, 82, 9, 86, 5, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97,
            82, 99, 76, 101, 70, 103, 66, 105, 106, 107, 66, 109, 60, 111, 56, 69,
            114, 65, 116, 61, 118, 57, 120, 53, 122, 49, 124, 43, 72, 39, 60, 33,
            56, 29, 52, 23, 48, 23, 42, 137, 38, 21, 140, 15, 142, 9, 144, 141,
            146, 147, 148, 149, 150, 151, 152, 153, 154, 155, 70, 157, 66, 81, 62, 75,
            58, 69, 54, 65, 50, 167, 44, 65, 40, 59, 34, 55, 30, 175, 24, 177,
            178, 179, 180, 181, 182, 183, 184, 69, 186, 59, 188, 55, 190, 51, 192, 47,
            194, 41, 196, 37, 198, 199, 72, 201, 62, 203, 58, 205, 54, 207, 50, 209,
            46, 211, 40, 213, 36, 215, 30, 217, 26, 219, 20, 71, 14, 61, 14, 57,
            8, 53, 228, 49, 230, 45, 232, 39, 234, 35, 138, 29, 24, 25, 240, 19,
            22, 13, 16, 13, 10, 7, 244, 249, 10, 89, 230
    };
    private static final int[] AFTER_LPS = {
            145, 4, 3, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13,
            14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29,
            30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45,
            46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61,
            62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77,
            78, 79, 80, 85, 226, 6, 176, 143, 138, 141, 112, 135, 104, 133, 100, 129,
            98, 127, 72, 125, 102, 123, 60, 121, 110, 119, 108, 117, 54, 115, 48, 113,
            134, 59, 132, 55, 130, 51, 128, 47, 126, 41, 62, 37, 66, 31, 54, 25,
            50, 131, 46, 17, 40, 15, 136, 7, 32, 139, 172, 9, 170, 85, 168, 248,
            166, 247, 164, 197, 162, 95, 160, 173, 158, 165, 156, 161, 60, 159, 56, 71,
            52, 163, 48, 59, 42, 171, 38, 169, 32, 53, 26, 47, 174, 193, 18, 191,
            222, 189, 218, 187, 216, 185, 214, 61, 212, 53, 210, 49, 208, 45, 206, 39,
            204, 195, 202, 31, 200, 243, 64, 239, 56, 237, 52, 235, 48, 233, 44, 231,
            38, 229, 34, 227, 28, 225, 22, 223, 16, 221, 220, 63, 8, 55, 224, 51,
            2, 47, 87, 43, 246, 37, 244, 33, 238, 27, 236, 21, 16, 15, 8, 241,
            242, 7, 10, 245, 2, 1, 83, 250, 2, 143, 246
    };

    private final byte[] input;
    private final int[] contexts = new int[CONTEXTS];
    private int position;
    private int current;
    private int bitsLeft;
    private int a;
    private int c;

    private BzzDecoder(byte[] input) {
        this.input = input;
        this.c = (nextByte() << 8) | nextByte();
    }

    /** The bytes `compressed` holds, failing when they would exceed `maxBytes`. */
    static byte[] decode(byte[] compressed, int maxBytes) throws TikaException {
        final BzzDecoder decoder = new BzzDecoder(compressed);
        final ByteArrayOutputStream out = new ByteArrayOutputStream();
        while (true) {
            final byte[] block = decoder.block();
            if (block == null) {
                return out.toByteArray();
            }
            if (out.size() + block.length > maxBytes) {
                throw new TikaException("BZZ data decompresses to more than " + maxBytes + " bytes");
            }
            out.write(block, 0, block.length);
        }
    }

    /** Bytes past the end of the input read as 0xff, as the specification requires. */
    private int nextByte() {
        return position < input.length ? input[position++] & 0xff : 0xff;
    }

    private int nextBit() {
        if (bitsLeft == 0) {
            current = nextByte();
            bitsLeft = 8;
        }
        bitsLeft--;
        return (current >> bitsLeft) & 1;
    }

    private void renormalize() {
        while (a >= 0x8000) {
            a = (a << 1) & 0xffff;
            c = ((c << 1) | nextBit()) & 0xffff;
        }
    }

    /**
     * One binary decision in context `i` (Figure 1). A decision that leaves the interval short
     * enough to need no renormalisation leaves its context's state as it is, as in the reference
     * coder that DjVu files are written with.
     */
    private int decode(int i) {
        final int state = contexts[i];
        int z = a + DELTA[state];
        final int d = 0x6000 + ((z + a) >> 2);
        if (z > d) {
            z = d;
        }
        if (z > c) {
            a = (a + 0x10000 - z) & 0xffff;
            c = (c + 0x10000 - z) & 0xffff;
            contexts[i] = AFTER_LPS[state];
            renormalize();
            return 1 - (state & 1);
        }
        if (z >= 0x8000 && a >= THRESHOLD[state]) {
            contexts[i] = AFTER_MPS[state];
        }
        a = z;
        renormalize();
        return state & 1;
    }

    /** One decision without a context, in the pass-through mode BZZ uses for its numbers. */
    private int decodePassThrough() {
        final int z = 0x8000 + (a >> 1);
        if (z > c) {
            a = (a + 0x10000 - z) & 0xffff;
            c = (c + 0x10000 - z) & 0xffff;
            renormalize();
            return 1;
        }
        a = z;
        renormalize();
        return 0;
    }

    private int decodeRaw(int bits) {
        int n = 1;
        while (n < (1 << bits)) {
            n = (n << 1) | decodePassThrough();
        }
        return n - (1 << bits);
    }

    private int decodeBin(int offset, int bits) {
        int n = 1;
        while (n < (1 << bits)) {
            n = (n << 1) | decode(offset + n - 1);
        }
        return n - (1 << bits);
    }

    /** The bytes of the next block, or null at the end of the stream (13.2.1.2). */
    private byte[] block() throws TikaException {
        final int blockSize = decodeRaw(24);
        if (blockSize == 0) {
            return null;
        }
        if (blockSize > MAX_BLOCK_BYTES) {
            throw new TikaException("A BZZ block of " + blockSize + " bytes exceeds the "
                    + MAX_BLOCK_BYTES + " the format allows");
        }
        int shift = 0;
        if (decodePassThrough() == 1) {
            shift = decodePassThrough() == 1 ? 2 : 1;
        }
        final int[] mtf = new int[256];
        for (int i = 0; i < 256; i++) {
            mtf[i] = i;
        }
        final int[] frequencies = new int[4];
        final byte[] data = new byte[blockSize];
        int markerPosition = -1;
        int add = 4;
        int rank = 3;
        for (int i = 0; i < blockSize; i++) {
            final int context = Math.min(rank, 2);
            if (decode(context) == 1) {
                rank = 0;
            } else if (decode(context + 3) == 1) {
                rank = 1;
            } else if (decode(6) == 1) {
                rank = 2 + decodeBin(7, 1);
            } else if (decode(8) == 1) {
                rank = 4 + decodeBin(9, 2);
            } else if (decode(12) == 1) {
                rank = 8 + decodeBin(13, 3);
            } else if (decode(20) == 1) {
                rank = 16 + decodeBin(21, 4);
            } else if (decode(36) == 1) {
                rank = 32 + decodeBin(37, 5);
            } else if (decode(68) == 1) {
                rank = 64 + decodeBin(69, 6);
            } else if (decode(132) == 1) {
                rank = 128 + decodeBin(133, 7);
            } else {
                rank = 256;
                data[i] = 0;
                markerPosition = i;
                continue;
            }
            final int symbol = mtf[rank];
            data[i] = (byte) symbol;
            add += add >> shift;
            if (add > 0x10000000) {
                add >>= 24;
                for (int j = 0; j < 4; j++) {
                    frequencies[j] >>= 24;
                }
            }
            final int frequency = rank < 4 ? add + frequencies[rank] : add;
            int k = rank;
            for (; k > 3; k--) {
                mtf[k] = mtf[k - 1];
            }
            for (; k > 0 && frequency >= frequencies[k - 1]; k--) {
                mtf[k] = mtf[k - 1];
                frequencies[k] = frequencies[k - 1];
            }
            mtf[k] = symbol;
            frequencies[k] = frequency;
        }
        if (markerPosition <= 0 || markerPosition >= blockSize) {
            throw new TikaException("A BZZ block has no valid end-of-block marker");
        }
        return inverseBurrowsWheeler(data, markerPosition);
    }

    /** Follows the transform's thread backwards (13.2.1.3). */
    private static byte[] inverseBurrowsWheeler(byte[] data, int markerPosition)
            throws TikaException {
        final int blockSize = data.length;
        final int[] count = new int[256];
        final int[] ranks = new int[blockSize];
        for (int i = 0; i < blockSize; i++) {
            final int symbol = data[i] & 0xff;
            if (i == markerPosition) {
                ranks[i] = 0;
            } else {
                ranks[i] = count[symbol]++;
            }
        }
        int last = 1;
        for (int i = 0; i < 256; i++) {
            final int occurrences = count[i];
            count[i] = last;
            last += occurrences;
        }
        if (last != blockSize) {
            throw new TikaException("A BZZ block does not add up to its size");
        }
        final byte[] decoded = new byte[blockSize - 1];
        int k = 0;
        for (last = blockSize - 1; last > 0; last--) {
            final int symbol = data[k] & 0xff;
            decoded[last - 1] = (byte) symbol;
            k = count[symbol] + ranks[k];
        }
        if (k != markerPosition) {
            throw new TikaException("A BZZ block's transform does not lead back to its marker");
        }
        return decoded;
    }
}
