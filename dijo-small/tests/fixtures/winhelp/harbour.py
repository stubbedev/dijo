"""Writes a Windows Help file as Manfred Winterhoff's notes on the format (helpdeco's
helpfile.txt) lay one out, with what WinHelpParser reads and what it skips:

- the help file header, and the directory, a B+ tree of one leaf page;
- |SYSTEM with the title, a copyright notice in Windows-1252 and, from WinHelp 3.1 on, an LCID
  (Danish, whose ANSI code page is Windows-1252);
- phrases, the old way in |Phrases or by Hall compression in |PhrIndex and |PhrImage;
- |TOPIC: topic headers, text records with a picture, a font change, a hotspot, line breaks, tabs
  and a non-breaking space, and from WinHelp 3.1 on a table of table records, one with a cell
  of two paragraphs and one skipping its first column;
- |KWBTREE, a B+ tree of two leaf pages under an index page, and |KWDATA;
- in WinHelp 3.0, |TOMAP, which numbers the topics.

Variants:

    winhelp4   WinHelp 4 (HCW 4.00): LZ77-compressed topic blocks of 4 KiB, old phrases
    hall       the same with Hall compression
    winhelp30  WinHelp 3.0 (HC30): uncompressed topic blocks of 2 KiB that the topic links run
               on across, uncompressed phrases, no table and no LCID

test_files/documents/harbour.hlp is the winhelp4 variant; dijo-small/tests/fixtures/winhelp/
holds hall.hlp and winhelp30.hlp.

Usage: harbour.py VARIANT OUTPUT.hlp
"""

import struct
import sys

WINDOW = 0x1000
MIN_MATCH = 3
MAX_MATCH = MIN_MATCH + 15


def lz77(data):
    """`data` LZ77-compressed greedily, each reference the longest match in the window."""
    out = bytearray()
    at = 0
    while at < len(data):
        flags_at = len(out)
        out.append(0)
        for bit in range(8):
            if at >= len(data):
                break
            best, best_from = 0, 0
            for start in range(max(0, at - WINDOW), at):
                length = 0
                while (
                    length < MAX_MATCH
                    and at + length < len(data)
                    and data[start + length] == data[at + length]
                ):
                    length += 1
                if length > best:
                    best, best_from = length, start
            if best >= MIN_MATCH:
                out[flags_at] |= 1 << bit
                out += struct.pack("<H", (best - MIN_MATCH) << 12 | (at - best_from - 1))
                at += best
            else:
                out.append(data[at])
                at += 1
    return bytes(out)


def compressed_u16(value):
    assert 0 <= value < 0x8000
    return bytes([value << 1]) if value < 0x80 else struct.pack("<H", value << 1 | 1)


def compressed_i16(value):
    if -0x40 <= value < 0x40:
        return bytes([(value + 0x40) << 1])
    return struct.pack("<H", (value + 0x4000) << 1 | 1)


def compressed_i32(value):
    if -0x4000 <= value < 0x4000:
        return struct.pack("<H", (value + 0x4000) << 1)
    return struct.pack("<I", (value + 0x40000000) << 1 | 1)


def stringz(text):
    return text.encode("cp1252") + b"\0"


# Phrases: the old way numbers them from 0; Hall compression's list puts "harbour" beyond the
# first 128, which it numbers in two bytes.
PHRASES = ["lighthouse", "keeper", "the lamp", "tide", "table"]
HALL_PHRASES = PHRASES + ["unused%03d" % n for n in range(125)] + ["harbour"]


def old_phrase_compress(text, phrases):
    """`text` with the phrases replaced the old way: twice the phrase's number, plus one when
    a space follows it, which goes with it."""
    out = bytearray()
    at = 0
    ordered = sorted(enumerate(phrases), key=lambda pair: -len(pair[1]))
    while at < len(text):
        for number, phrase in ordered:
            encoded = phrase.encode("cp1252")
            if text.startswith(encoded, at):
                code = 2 * number
                at += len(encoded)
                if text[at : at + 1] == b" ":
                    code += 1
                    at += 1
                out += bytes([(code >> 8) + 1, code & 0xFF])
                break
        else:
            assert not 1 <= text[at] < 16
            out.append(text[at])
            at += 1
    return bytes(out)


def hall_compress(text, phrases):
    """`text` in Hall compression's codes: phrases, runs of NULs, of spaces and of bytes."""
    encoded = sorted(
        ((number, phrase.encode("cp1252")) for number, phrase in enumerate(phrases)),
        key=lambda pair: -len(pair[1]),
    )

    def phrase_at(at):
        for number, phrase in encoded:
            if text.startswith(phrase, at):
                return number, phrase
        return None

    out = bytearray()
    at = 0
    while at < len(text):
        found = phrase_at(at)
        if found:
            number, phrase = found
            if number < 128:
                out.append(number * 2)
            else:
                number -= 128
                out += bytes([(number >> 8) << 2 | 1, number & 0xFF])
            at += len(phrase)
        elif text[at] in b"\0 ":
            byte = text[at]
            run = 1
            while run < 16 and text[at + run : at + run + 1] == bytes([byte]):
                run += 1
            out.append((run - 1) << 4 | (0x0F if byte == 0 else 0x07))
            at += run
        else:
            run = 1
            while (
                run < 32
                and at + run < len(text)
                and text[at + run] not in b"\0 "
                and not phrase_at(at + run)
            ):
                run += 1
            out.append((run - 1) << 3 | 3)
            out += text[at : at + run]
            at += run
    return bytes(out)


class Variant:
    def __init__(self, name):
        self.name = name
        self.win30 = name == "winhelp30"
        self.hall = name == "hall"
        self.phrases = HALL_PHRASES if self.hall else PHRASES

    def compress_text(self, text):
        """The stored second part of a topic link for `text`: phrase-compressed where that is
        shorter."""
        if self.hall:
            stored = hall_compress(text, self.phrases)
        else:
            stored = old_phrase_compress(text, self.phrases)
        return stored if len(stored) < len(text) else text


# Commands of the text records.
END, FONT, LINE_BREAK, END_OF_PARAGRAPH, TAB = 0xFF, 0x80, 0x81, 0x82, 0x83
PICTURE, END_OF_HOTSPOT, NON_BREAKING_SPACE, JUMP = 0x86, 0x89, 0x8B, 0xE3


class Paragraphs:
    """The strings and commands of the paragraphs of a record."""

    def __init__(self):
        self.strings = []
        self.current = b""
        self.commands = bytearray()

    def text(self, text):
        self.current += text.encode("cp1252")
        return self

    def command(self, command):
        self.strings.append(self.current)
        self.current = b""
        self.commands += command
        return self

    def font(self, number):
        return self.command(bytes([FONT]) + struct.pack("<h", number))

    def picture(self):
        """A {bmc} of the picture |bm0: no hotspots, not embedded, picture number 0."""
        union = struct.pack("<hh", 0, 0)
        return self.command(
            bytes([PICTURE, 0x22]) + compressed_i32(len(union)) + compressed_u16(0) + union
        )

    def jump(self, text, context_hash):
        self.command(bytes([JUMP]) + struct.pack("<i", context_hash))
        self.text(text)
        return self.command(bytes([END_OF_HOTSPOT]))

    def simple(self, command):
        return self.command(bytes([command]))

    def end_paragraph(self):
        return self.simple(END_OF_PARAGRAPH)

    def end(self):
        return self.simple(END)

    def text_bytes(self):
        assert self.current == b""
        return b"".join(string + b"\0" for string in self.strings)


def paragraph_format(tabs=False):
    """Four unknown bytes, the flags and what they say follows: spacing above and a left
    indent always, and a border and two tab stops, one right-aligned, where `tabs`."""
    flags = 0x0002 | 0x0010
    if tabs:
        flags |= 0x0100 | 0x0200
    out = bytes([0, 0x80]) + struct.pack("<H", 0) + struct.pack("<H", flags)
    out += compressed_i16(6) + compressed_i16(720)
    if tabs:
        out += bytes([0x01]) + struct.pack("<h", 1)
        out += compressed_i16(2) + compressed_u16(1440) + compressed_u16(2880 | 0x4000)
        out += compressed_u16(1)
    return out


TOPIC_HEADER, DISPLAY_30, DISPLAY, TABLE = 0x02, 0x01, 0x20, 0x23


def display(variant, paragraphs, tabs=False):
    """A text record of `paragraphs`."""
    text = paragraphs.text_bytes()
    data1 = compressed_i32(len(text))
    if not variant.win30:
        data1 += compressed_u16(len(text))
    data1 += paragraph_format(tabs) + bytes(paragraphs.commands)
    return (DISPLAY_30 if variant.win30 else DISPLAY, data1, text)


def table_row(cells, columns=2):
    """A table record of a row whose `cells` are (column, Paragraphs) pairs."""
    strings = b""
    data1 = bytes([columns, 0]) + struct.pack("<h", 4000)
    for _ in range(columns):
        data1 += struct.pack("<hh", 100, 2000)
    for column, paragraphs in cells:
        data1 += struct.pack("<hhB", column, 0, 0) + paragraph_format()
        data1 += bytes(paragraphs.commands)
        strings += paragraphs.text_bytes()
    data1 += struct.pack("<h", -1)
    head = compressed_i32(len(strings)) + compressed_u16(len(strings))
    return (TABLE, head + data1, strings)


def cell(*texts):
    paragraphs = Paragraphs()
    for text in texts:
        paragraphs.text(text).end_paragraph()
    return paragraphs.end()


def topics(variant):
    """The records of the topics, a topic header starting each."""
    records = []

    def header(title):
        records.append((TOPIC_HEADER, None, stringz(title)))

    header("The Lighthouse")
    records.append(
        display(
            variant,
            Paragraphs()
            .picture()
            .font(0)
            .text("The lighthouse keeper lights the lamp at dusk.")
            .end_paragraph()
            .end(),
        )
    )
    records.append(
        display(
            variant,
            Paragraphs()
            .font(1)
            .text("Lamp:")
            .simple(TAB)
            .text("Fresnel lens")
            .simple(LINE_BREAK)
            .text("Range:")
            .simple(TAB)
            .text("18")
            .simple(NON_BREAKING_SPACE)
            .text("nautical miles")
            .end_paragraph()
            .text("Fyret lyser om natten, og Ove Grøn holder vagt. ")
            .jump("See the tide table", 0x2F6D3A1)
            .text(".")
            .end_paragraph()
            .end(),
            tabs=True,
        )
    )
    if variant.win30:
        header("Keeper's Log")
        log = Paragraphs()
        for day in range(1, 61):
            log.text("Day %d: the lamp was lit at dusk and the harbour was calm." % day)
            log.simple(LINE_BREAK)
        records.append(display(variant, log.text("End of the log.").end_paragraph().end()))
        return records
    header("Tide Table")
    records.append(
        display(
            variant,
            Paragraphs().text("The harbour tides for the first of May.").end_paragraph().end(),
        )
    )
    records.append(table_row([(0, cell("Hour")), (1, cell("Height"))]))
    records.append(table_row([(0, cell("06:00")), (1, cell("1.2 m"))]))
    records.append(table_row([(0, cell("18:00")), (1, cell("1.4 m", "Spring tide"))]))
    records.append(table_row([(1, cell("calm"))]))
    records.append(
        display(
            variant,
            Paragraphs()
            .text("Tides are given in metres above chart datum.")
            .end_paragraph()
            .end(),
        )
    )
    return records


LINK_HEADER = 21
BLOCK_HEADER = 12


def link(variant, record_type, data1, text, previous, next_link):
    stored = variant.compress_text(text)
    return (
        struct.pack(
            "<iiiiiB",
            LINK_HEADER + len(data1) + len(stored),
            len(text),
            previous,
            next_link,
            LINK_HEADER + len(data1),
            record_type,
        )
        + data1
        + stored
    )


def topic_header_data(variant, number):
    if variant.win30:
        return struct.pack("<iii", 0, -1, -1)
    return struct.pack("<iiiiiii", 0, -1, -1, number, -1, -1, -1)


def topic_file(variant):
    """|TOPIC: the links laid out in the blocks' data, where their positions are known, and
    the positions of the topic headers."""
    records = topics(variant)
    number = 0
    filled = []
    for record_type, data1, text in records:
        if data1 is None:
            data1 = topic_header_data(variant, number)
            number += 1
        filled.append((record_type, data1, text))
    lengths = [len(link(variant, kind, data1, text, 0, 0)) for kind, data1, text in filled]
    if variant.win30:
        data_size = 0x800 - BLOCK_HEADER

        def position(offset):
            return offset // data_size * 0x800 + BLOCK_HEADER + offset % data_size

    else:
        data_size = 0x4000

        def position(offset):
            return BLOCK_HEADER + offset

    offsets = [0]
    for length in lengths:
        offsets.append(offsets[-1] + length)
    stream = bytearray()
    headers = [
        position(offsets[index])
        for index, (kind, _, _) in enumerate(filled)
        if kind == TOPIC_HEADER
    ]
    for index, (kind, data1, text) in enumerate(filled):
        here = position(offsets[index])
        following = position(offsets[index + 1])
        previous = position(offsets[index - 1]) if index else -1
        if variant.win30:
            next_link = following - here
            previous = here - previous if index else -1
        else:
            next_link = following
        stream += link(variant, kind, data1, text, previous, next_link)
    last = position(offsets[-1])
    if variant.win30:
        end = position(offsets[-1] + LINK_HEADER)
        stream += struct.pack("<iiiiiB", LINK_HEADER, 0, 0, end - last, LINK_HEADER, 2)
    else:
        stream += struct.pack("<iiiiiB", LINK_HEADER, 0, 0, -1, LINK_HEADER, 2)
    out = bytearray()
    for block, start in enumerate(range(0, len(stream), data_size)):
        data = bytes(stream[start : start + data_size])
        header = struct.pack("<iii", -1 if block == 0 else 0, BLOCK_HEADER, 0)
        if not variant.win30:
            data = lz77(data)
            assert len(data) <= 0x1000 - BLOCK_HEADER
        out += header + data
    return bytes(out), headers


def tomap(headers):
    """|TOMAP of WinHelp 3.0: the position of the contents topic, then those of the topics by
    their numbers, which start at 16."""
    return struct.pack("<%di" % (16 + len(headers)), headers[0], *[0] * 15, *headers)


def system_file(variant):
    if variant.win30:
        return struct.pack("<HHHIH", 0x036C, 15, 1, 0, 0) + stringz("Harbour Help")
    out = struct.pack("<HHHIH", 0x036C, 33, 1, 0, 4)
    for kind, data in [
        (1, stringz("Harbour Help")),
        (2, stringz("© 2026 Harbour Office")),
        (9, struct.pack("<HHHHH", 0, 0, 0, 0, 0x0406)),
        (3, struct.pack("<i", 0)),
    ]:
        out += struct.pack("<HH", kind, len(data)) + data
    return out


def phrases_file(variant):
    text = b"".join(phrase.encode("cp1252") for phrase in variant.phrases)
    count = len(variant.phrases)
    offsets = [2 * (count + 1)]
    for phrase in variant.phrases:
        offsets.append(offsets[-1] + len(phrase))
    packed = struct.pack("<%dH" % len(offsets), *offsets)
    if variant.win30:
        return struct.pack("<HH", count, 0x0100) + packed + text
    return struct.pack("<HHI", count, 0x0100, len(text)) + packed + lz77(text)


def hall_files(variant):
    """|PhrIndex, the phrases' lengths in bits, and |PhrImage, their text LZ77-compressed."""
    bit_count = 4
    bits = []
    for phrase in variant.phrases:
        rest = len(phrase) - 1
        bits += [1] * (rest >> bit_count) + [0]
        bits += [rest >> bit & 1 for bit in range(bit_count)]
    bits += [0] * (-len(bits) % 32)
    bits += [0] * len(variant.phrases)
    bits += [0] * (-len(bits) % 32)
    packed = bytes(
        sum(bit << shift for shift, bit in enumerate(bits[at : at + 8]))
        for at in range(0, len(bits), 8)
    )
    text = b"".join(phrase.encode("cp1252") for phrase in variant.phrases)
    image = lz77(text)
    assert len(image) != len(text)
    index = struct.pack(
        "<iiiiiiHH", 1, len(variant.phrases), len(packed), len(text), len(image), 0,
        bit_count, 0x4A00,
    ) + packed
    return index, image


def btree(structure, page_size, levels, pages, entries):
    header = struct.pack(
        "<HHH16shhhhhhi",
        0x293B,
        0x0002 | (0x0400 if structure == b"z4" else 0),
        page_size,
        structure,
        0,
        0,
        len(pages) - 1 if levels > 1 else 0,
        -1,
        len(pages),
        levels,
        entries,
    )
    return header + b"".join(page.ljust(page_size, b"\0") for page in pages)


def leaf_page(page_size, entries, previous, following):
    body = b"".join(entries)
    return struct.pack("<Hhhh", page_size - 8 - len(body), len(entries), previous, following) + body


def directory(files):
    entries = [stringz(name) + struct.pack("<i", offset) for name, offset in sorted(files)]
    return btree(b"z4", 0x400, 1, [leaf_page(0x400, entries, -1, -1)], len(entries))


def keyword_files():
    """|KWBTREE of two leaf pages under an index page, and |KWDATA, one topic offset each."""
    page_size = 0x800
    keywords = ["harbour", "lamp", "lighthouse", "tides"]
    entries = [
        stringz(keyword) + struct.pack("<hi", 1, 4 * index)
        for index, keyword in enumerate(keywords)
    ]
    first = leaf_page(page_size, entries[:2], -1, 1)
    second = leaf_page(page_size, entries[2:], 0, -1)
    index_body = stringz(keywords[2]) + struct.pack("<h", 1)
    index = struct.pack("<Hhh", page_size - 6 - len(index_body), 1, 0) + index_body
    tree = btree(b"F24", page_size, 2, [first, second, index], len(keywords))
    data = b"".join(struct.pack("<i", 0x8000 * (index // 2)) for index in range(len(keywords)))
    return tree, data


def help_file(variant):
    topic, headers = topic_file(variant)
    files = [("|SYSTEM", system_file(variant)), ("|TOPIC", topic)]
    if variant.win30:
        files.append(("|TOMAP", tomap(headers)))
    if variant.hall:
        index, image = hall_files(variant)
        files += [("|PhrIndex", index), ("|PhrImage", image)]
    else:
        files.append(("|Phrases", phrases_file(variant)))
    tree, data = keyword_files()
    files += [("|KWBTREE", tree), ("|KWDATA", data)]
    out = bytearray(16)
    placed = []
    for name, content in files:
        placed.append((name, len(out)))
        out += struct.pack("<iiB", len(content) + 9, len(content), 0) + content
    directory_start = len(out)
    listing = directory(placed)
    out += struct.pack("<iiB", len(listing) + 9, len(listing), 4) + listing
    out[0:16] = struct.pack("<Iiii", 0x00035F3F, directory_start, -1, len(out))
    return bytes(out)


def main():
    variant, output = sys.argv[1:]
    if variant not in ("winhelp4", "hall", "winhelp30"):
        sys.exit(__doc__)
    with open(output, "wb") as file:
        file.write(help_file(Variant(variant)))


if __name__ == "__main__":
    main()
