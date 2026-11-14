"""Writes the LZFSE streams the unit tests of AppleDiskImageParser's LZFSE decoder read, encoded
by Apple's reference encoder, lzfse 1.0, whose command is given:

- lzvn.lzfse: 2,000 bytes of text, which the encoder writes as one LZVN block (bvxn);
- raw.lzfse: 64 bytes that do not compress, written as one uncompressed block (bvx-);
- blocks.lzfse: 200,000 bytes of text, written as several LZFSE blocks with compressed headers
  (bvx2), whose matches reach back into the blocks before;
- sectors.lzfse: 32,768 bytes of text, 64 sectors, as a disk image's LZFSE chunk decodes
  into, written as LZFSE blocks;
- v1.lzfse: blocks.lzfse with each header rewritten uncompressed (bvx1), as the reference
  decoder reads both and its encoder writes only the compressed form.

Each stream is printed with the length and CRC-32 of what it decodes into.

Usage: lzfse.py LZFSE-COMMAND OUTPUT-DIRECTORY
"""

import os
import struct
import subprocess
import sys
import tempfile
import zlib

WORDS = (
    "lamp keeper tower storm dusk dawn wick oil lens beacon harbour gale tide rock "
    "light ship signal fog bell north west reef channel watch night shore"
).split()

L_SYMBOLS, M_SYMBOLS, D_SYMBOLS, LITERAL_SYMBOLS = 20, 20, 64, 256
FREQ_NBITS = [2, 3, 2, 5, 2, 3, 2, 8, 2, 3, 2, 5, 2, 3, 2, 14] * 2
FREQ_VALUE = [0, 2, 1, 4, 0, 3, 1, -1, 0, 2, 1, 5, 0, 3, 1, -1,
              0, 2, 1, 6, 0, 3, 1, -1, 0, 2, 1, 7, 0, 3, 1, -1]


def lcg(seed):
    state = seed
    while True:
        state = (state * 1103515245 + 12345) & 0x7FFFFFFF
        yield state >> 8


def text(length, seed):
    numbers = lcg(seed)
    out = []
    size = 0
    line = 0
    while size < length:
        words = " ".join(WORDS[next(numbers) % len(WORDS)] for _ in range(4 + next(numbers) % 9))
        sentence = f"Entry {line}: the {words}.\n"
        out.append(sentence)
        size += len(sentence)
        line += 1
    return "".join(out).encode()[:length]


def noise(length, seed):
    numbers = lcg(seed)
    return bytes(next(numbers) & 0xFF for _ in range(length))


def encode(command, data):
    with tempfile.TemporaryDirectory() as directory:
        source = os.path.join(directory, "in")
        target = os.path.join(directory, "out")
        with open(source, "wb") as handle:
            handle.write(data)
        subprocess.run([command, "-encode", "-i", source, "-o", target], check=True)
        with open(target, "rb") as handle:
            return handle.read()


def freq_tables(header, size):
    """The frequency tables of a bvx2 header, decoded as lzfse_decode_v1 decodes them."""
    tables = []
    accum = 0
    accum_bits = 0
    at = 32
    for _ in range(L_SYMBOLS + M_SYMBOLS + D_SYMBOLS + LITERAL_SYMBOLS):
        while at < size and accum_bits + 8 <= 32:
            accum |= header[at] << accum_bits
            accum_bits += 8
            at += 1
        bits = accum & 31
        nbits = FREQ_NBITS[bits]
        if nbits == 8:
            value = 8 + ((accum >> 4) & 0xF)
        elif nbits == 14:
            value = 24 + ((accum >> 4) & 0x3FF)
        else:
            value = FREQ_VALUE[bits]
        assert nbits <= accum_bits
        accum >>= nbits
        accum_bits -= nbits
        tables.append(value)
    assert at == size and accum_bits < 8
    return tables


def field(value, offset, bits):
    return (value >> offset) & ((1 << bits) - 1)


def to_v1(stream):
    """The stream with each bvx2 header rewritten as the bvx1 header it stands for."""
    out = bytearray()
    at = 0
    while True:
        magic = stream[at:at + 4]
        if magic == b"bvx$":
            return bytes(out + magic)
        assert magic == b"bvx2", magic
        raw, = struct.unpack_from("<I", stream, at + 4)
        v0, v1, v2 = struct.unpack_from("<QQQ", stream, at + 8)
        size = field(v2, 0, 32)
        literal_payload = field(v0, 20, 20)
        lmd_payload = field(v1, 40, 20)
        freqs = freq_tables(stream[at:at + size], size)
        header = struct.pack(
            "<4sIIIIIIi4HiHHH",
            b"bvx1",
            raw,
            literal_payload + lmd_payload,
            field(v0, 0, 20),
            field(v0, 40, 20),
            literal_payload,
            lmd_payload,
            field(v0, 60, 3) - 7,
            field(v1, 0, 10),
            field(v1, 10, 10),
            field(v1, 20, 10),
            field(v1, 30, 10),
            field(v1, 60, 3) - 7,
            field(v2, 32, 10),
            field(v2, 42, 10),
            field(v2, 52, 10),
        )
        header += struct.pack(f"<{len(freqs)}H", *freqs)
        header += b"\0\0"
        assert len(header) == 772
        payload = stream[at + size:at + size + literal_payload + lmd_payload]
        out += header + payload
        at += size + literal_payload + lmd_payload


def main():
    command, directory = sys.argv[1], sys.argv[2]
    inputs = {
        "lzvn": text(2000, 1),
        "raw": noise(64, 2),
        "blocks": text(200_000, 3),
        "sectors": text(32_768, 4),
    }
    streams = {name: encode(command, data) for name, data in inputs.items()}
    streams["v1"] = to_v1(streams["blocks"])
    inputs["v1"] = inputs["blocks"]
    for name, stream in streams.items():
        with open(os.path.join(directory, f"{name}.lzfse"), "wb") as handle:
            handle.write(stream)
        data = inputs[name]
        print(f"{name}.lzfse: {len(stream)} bytes, {stream[:4]}, decodes into "
              f"{len(data)} bytes of CRC-32 {zlib.crc32(data):#010x}")


main()
