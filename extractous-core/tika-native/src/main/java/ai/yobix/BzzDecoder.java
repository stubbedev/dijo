package ai.yobix;

import org.apache.tika.exception.TikaException;

import java.io.ByteArrayOutputStream;

/**
 * Decompresses BZZ, the general-purpose compression of DjVu files: blocks of a Burrows-Wheeler
 * transform whose move-to-front ranks are coded with the Z′ adaptive binary arithmetic coder,
 * as the DjVu v3 specification describes them (Appendix 4).
 */
final class BzzDecoder {

    private static final int MAX_BLOCK_BYTES = 4096 * 1024;
    private static final int CONTEXTS = 260;

    private final ZpDecoder zp;
    private final int[] contexts = new int[CONTEXTS];

    private BzzDecoder(byte[] input) {
        this.zp = new ZpDecoder(input);
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

    private int decode(int context) {
        return zp.decode(contexts, context);
    }

    private int decodePassThrough() {
        return zp.decodePassThrough();
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
