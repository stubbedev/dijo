"""Writes test_files/documents/cabinet.cab, the first cabinet of a set of two ([MS-CAB]), with a
folder of each kind CabParser reads and of each it does not:

0. stored: readme.txt, and a file whose name is UTF-8 (_A_NAME_IS_UTF);
1. MSZIP ([MS-MCI] 2.1), deflated by zlib a 32 KiB block at a time, each block given the 32 KiB
   before it as its dictionary: a binary file of three blocks, then mszip.txt;
2. LZX with a 64 KiB window and E8 call translation, encoded here a 32 KiB frame per block, each
   frame one verbatim block with fixed trees and greedy matches: a binary file of E8 calls,
   then lzx.txt;
3. Quantum, which is not decoded: its data is stored bytes, flagged as Quantum;
4. stored, continued by the next cabinet, cabinet2.cab, which is not written: whole.txt, then
   spanned.txt, which ends there (ifoldCONTINUED_TO_NEXT).

cabextract 1.11 extracts the files of folders 0 to 2 and whole.txt as written here.

Usage: cabinet.py OUTPUT.cab
"""

import struct
import sys
import zlib

FRAME = 32 * 1024
DATE = ((2024 - 1980) << 9) | (5 << 5) | 17
TIME = (14 << 11) | (30 << 5) | (20 >> 1)
E8_TRANSLATION_SIZE = 12_000_000

README = (
    b"This cabinet was written to test how dijo reads Microsoft Cabinet archives.\r\n"
    b"Each file in it is handed on as an embedded document.\r\n"
)
UNICODE = "Caf\u00e9 notes: the name of this file is UTF-8 encoded.\n".encode()
MSZIP_TEXT = b"The deflated folder carries its window from one block to the next.\n"
LZX_TEXT = b"The LZX folder is decoded frame by frame, with its calls translated back.\n"
QUANTUM_TEXT = b"Quantum is not decoded, so this sentence is never read.\n"
WHOLE = b"This file ends in the first cabinet of the set.\n"
SPANNED = b"This file begins in the first cabinet and ends in the second.\n"


def table(length):
    """Binary data that compresses: a table of little-endian counters and zero padding."""
    rows = bytearray()
    row = 0
    while len(rows) < length:
        rows += struct.pack("<IIHH", row, row * 7 % 1000, row % 13, 0)
        row += 1
    return bytes(rows[:length])


def calls(length):
    """x86-like code: CALL instructions (E8) to addresses within and beyond the translation
    size, among filler bytes."""
    code = bytearray()
    index = 0
    while len(code) < length:
        target = (0x40 + index * 0x31) if index % 3 else -(0x7000_0000 + index)
        code += b"\x55\x8b\xec\xe8" + struct.pack("<i", target) + b"\x5d\xc3\x90"
        index += 1
    return bytes(code[:length])


class Bits:
    """LZX's bitstream: 16-bit little-endian words, each filled from its most significant bit."""

    def __init__(self):
        self.out = bytearray()
        self.word = 0
        self.count = 0

    def put(self, value, length):
        for shift in range(length - 1, -1, -1):
            self.word = (self.word << 1) | ((value >> shift) & 1)
            self.count += 1
            if self.count == 16:
                self.out += struct.pack("<H", self.word)
                self.word = 0
                self.count = 0

    def align(self):
        if self.count:
            self.put(0, 16 - self.count)


def canonical(lengths):
    codes = [0] * len(lengths)
    code = 0
    for length in range(1, 17):
        for symbol, symbol_length in enumerate(lengths):
            if symbol_length == length:
                codes[symbol] = code
                code += 1
        code <<= 1
    return codes


def flat(count, short, long):
    """Path lengths of a complete code of `count` symbols, of two lengths."""
    shorter = (1 << long) - count
    assert 0 <= shorter <= count
    return [short] * shorter + [long] * (count - shorter)


EXTRA_BITS = []
for slot in range(0, 52, 2):
    bits = min(max(slot // 2 - 1, 0), 17)
    EXTRA_BITS += [bits, bits]
POSITION_BASE = [0]
for bits in EXTRA_BITS[:-1]:
    POSITION_BASE.append(POSITION_BASE[-1] + (1 << bits))

WINDOW_BITS = 16
POSITION_SLOTS = 32
PRETREE = [4] * 12 + [5] * 8
MAIN = flat(256 + 8 * POSITION_SLOTS, 9, 9)
LENGTH = flat(249, 7, 8)


class Lzx:
    def __init__(self):
        self.previous_main = [0] * len(MAIN)
        self.previous_length = [0] * len(LENGTH)
        self.history = b""
        self.first = True

    def lengths(self, bits, previous, lengths):
        codes = canonical(PRETREE)
        for length in PRETREE:
            bits.put(length, 4)
        for old, new in zip(previous, lengths):
            symbol = (old - new) % 17
            bits.put(codes[symbol], PRETREE[symbol])

    def frame(self, frame):
        """One frame, already E8-translated, as one verbatim block, the frame's bitstream
        aligned to its words."""
        bits = Bits()
        if self.first:
            bits.put(1, 1)
            bits.put(E8_TRANSLATION_SIZE, 32)
            self.first = False
        bits.put(1, 3)
        bits.put(len(frame), 24)
        self.lengths(bits, self.previous_main[:256], MAIN[:256])
        self.lengths(bits, self.previous_main[256:], MAIN[256:])
        self.lengths(bits, self.previous_length, LENGTH)
        self.previous_main, self.previous_length = MAIN, LENGTH
        main_codes, length_codes = canonical(MAIN), canonical(LENGTH)
        data = self.history + frame
        start = len(self.history)
        window = (1 << WINDOW_BITS) - 3
        recent = {}
        for at in range(max(0, start - window), start):
            recent[data[at : at + 3]] = at
        at = start
        while at < len(data):
            candidate = recent.get(data[at : at + 3])
            length = 0
            if candidate is not None and at - candidate <= window:
                limit = min(257, len(data) - at)
                while length < limit and data[candidate + length] == data[at + length]:
                    length += 1
            if length >= 3:
                formatted = at - candidate + 2
                slot = max(s for s in range(POSITION_SLOTS) if POSITION_BASE[s] <= formatted)
                header = min(length - 2, 7)
                symbol = 256 + slot * 8 + header
                bits.put(main_codes[symbol], MAIN[symbol])
                if header == 7:
                    bits.put(length_codes[length - 9], LENGTH[length - 9])
                bits.put(formatted - POSITION_BASE[slot], EXTRA_BITS[slot])
            else:
                length = 1
                bits.put(main_codes[data[at]], MAIN[data[at]])
            for covered in range(at, at + length):
                recent[data[covered : covered + 3]] = covered
            at += length
        bits.align()
        self.history = data[-(1 << WINDOW_BITS) :]
        return bytes(bits.out)


def e8_translated(frame, offset):
    """The frame as an LZX encoder gives it before compressing it: each call's relative target
    made absolute, as the decoder makes it relative again."""
    data = bytearray(frame)
    if len(data) <= 10:
        return bytes(data)
    at = 0
    while at < len(data) - 10:
        if data[at] != 0xE8:
            at += 1
            continue
        current = offset + at
        relative = struct.unpack_from("<i", data, at + 1)[0]
        if -current <= relative < E8_TRANSLATION_SIZE - current:
            absolute = relative + current
        elif E8_TRANSLATION_SIZE - current <= relative < E8_TRANSLATION_SIZE:
            absolute = relative - E8_TRANSLATION_SIZE
        else:
            absolute = relative
        assert absolute != 0
        struct.pack_into("<i", data, at + 1, absolute)
        at += 5
    return bytes(data)


def frames(data):
    return [data[at : at + FRAME] for at in range(0, len(data), FRAME)]


def stored(data):
    return [(frame, len(frame)) for frame in frames(data)]


def mszip(data):
    blocks = []
    for index, frame in enumerate(frames(data)):
        dictionary = data[max(0, index * FRAME - FRAME) : index * FRAME]
        options = {"zdict": dictionary} if dictionary else {}
        deflater = zlib.compressobj(9, zlib.DEFLATED, -15, 9, **options)
        blocks.append((b"CK" + deflater.compress(frame) + deflater.flush(), len(frame)))
    return blocks


def lzx(data):
    encoder = Lzx()
    return [
        (encoder.frame(e8_translated(frame, index * FRAME)), len(frame))
        for index, frame in enumerate(frames(data))
    ]


def folder(type_compress, files, encode):
    data = b"".join(content for _, content, _ in files)
    entries = []
    offset = 0
    for name, content, attributes in files:
        entries.append((name, len(content), offset, attributes))
        offset += len(content)
    return type_compress, encode(data), entries


def cabinet():
    utf8 = 0x80
    folders = [
        folder(
            0x0000,
            [
                (b"readme.txt", README, 0x20),
                ("notes\\caf\u00e9 \u65e5\u672c.txt".encode(), UNICODE, 0x20 | utf8),
            ],
            stored,
        ),
        folder(
            0x0001,
            [(b"data\\table.bin", table(3 * FRAME + 1000), 0x20), (b"mszip.txt", MSZIP_TEXT, 0x20)],
            mszip,
        ),
        folder(
            0x0003 | (WINDOW_BITS << 8),
            [(b"program.bin", calls(FRAME + 5000), 0x20), (b"lzx.txt", LZX_TEXT, 0x20)],
            lzx,
        ),
        folder(0x0002 | (4 << 4) | (21 << 8), [(b"quantum.txt", QUANTUM_TEXT, 0x20)], stored),
    ]
    split = len(WHOLE) + 20
    last_data = WHOLE + SPANNED
    last_blocks = [(last_data[:split], split), (last_data[split:], 0)]
    folders.append(
        (
            0x0000,
            last_blocks,
            [(b"whole.txt", len(WHOLE), 0, 0x20), (b"spanned.txt", len(SPANNED), len(WHOLE), 0x20)],
        )
    )
    next_cabinet = b"cabinet2.cab\0Disk 2\0"
    files_at = 36 + len(next_cabinet) + 8 * len(folders)
    file_headers = bytearray()
    for index, (_, _, entries) in enumerate(folders):
        last = index == len(folders) - 1
        for name, size, offset, attributes in entries:
            place = 0xFFFE if last and name == b"spanned.txt" else index
            file_headers += struct.pack("<IIHHHH", size, offset, place, DATE, TIME, attributes)
            file_headers += name + b"\0"
    folder_headers = bytearray()
    blocks = bytearray()
    data_at = files_at + len(file_headers)
    for type_compress, encoded, _ in folders:
        folder_headers += struct.pack("<IHH", data_at + len(blocks), len(encoded), type_compress)
        for data, decoded in encoded:
            blocks += struct.pack("<IHH", 0, len(data), decoded) + data
    count = sum(len(entries) for _, _, entries in folders)
    length = data_at + len(blocks)
    header = struct.pack(
        "<4sIIIIIBBHHHHH", b"MSCF", 0, length, 0, files_at, 0, 3, 1, len(folders), count, 0x0002,
        0x2026, 0,
    )
    return header + next_cabinet + folder_headers + file_headers + blocks


def main():
    with open(sys.argv[1], "wb") as output:
        output.write(cabinet())


if __name__ == "__main__":
    main()
