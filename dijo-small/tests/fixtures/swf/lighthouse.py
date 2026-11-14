"""Writes test_files/documents/lighthouse.swf, a Flash movie of SWF 10 as Flash writes one, with
what SwfParser reads and what it leaves out, and the same movie uncompressed and LZMA-compressed:

- FileAttributes with the ActionScript 3 flag, and the Metadata tag's XMP: a title, a creator and
  a description;
- a DefineFont3 and a DefineFont2 with code tables of two-byte codes, and a DefineFont whose
  codes a DefineFontInfo2 gives;
- a DefineText of three lines: a record for each line, a change of font within the second line,
  and a run moved along the third line past a gap, which reads as a space;
- a DefineText2 set in the DefineFont, with a character beyond ASCII;
- a DefineEditText of HTML text, with paragraphs, a line break, formatting, a link and two list
  items, a DefineEditText of plain text of two lines, and a password field, which is left out;
- a DefineBitsJPEG2 of an 8 by 8 grey JPEG, with the erroneous header Flash writes before it,
  and a DefineSound of two silent MP3 frames;
- a DefineSprite placing the static text, PlaceObject2 and ShowFrame tags, and a FrameLabel.

The glyph shapes are empty, as nothing that reads text draws them.

Usage: lighthouse.py OUTPUT_DIRECTORY
"""

import lzma
import os
import struct
import sys
import zlib

VERSION = 10

END = 0
SHOW_FRAME = 1
DEFINE_TEXT = 11
DEFINE_FONT = 10
DEFINE_SOUND = 14
DEFINE_BITS_JPEG_2 = 21
PLACE_OBJECT_2 = 26
DEFINE_TEXT_2 = 33
DEFINE_EDIT_TEXT = 37
DEFINE_SPRITE = 39
FRAME_LABEL = 43
DEFINE_FONT_2 = 48
DEFINE_FONT_INFO_2 = 62
FILE_ATTRIBUTES = 69
DEFINE_FONT_3 = 75
METADATA = 77
SET_BACKGROUND_COLOR = 9

XMP = """<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF \
xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" \
xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title><rdf:Alt><rdf:li xml:lang="x-default">\
The Lighthouse</rdf:li></rdf:Alt></dc:title><dc:creator><rdf:Seq><rdf:li>Anna Berg</rdf:li>\
</rdf:Seq></dc:creator><dc:description><rdf:Alt><rdf:li xml:lang="x-default">A banner for the \
harbour office.</rdf:li></rdf:Alt></dc:description></rdf:Description></rdf:RDF></x:xmpmeta>"""

HTML = (
    '<p align="left"><font face="Arial" size="14" color="#000000">Keeper\'s log</font></p>'
    '<p align="left"><font face="Arial" size="12">Oil delivered on <b>Mondays</b>.<br>'
    'Wicks trimmed at <a href="http://example.org/" target="_blank">dusk</a>.</font></p>'
    "<li>Lamp</li><li>Lens</li>"
)


class Bits:
    """Bit fields packed most significant bit first, as SWF packs them."""

    def __init__(self):
        self.out = bytearray()
        self.value = 0
        self.count = 0

    def put(self, value, bits):
        value &= (1 << bits) - 1
        for shift in range(bits - 1, -1, -1):
            self.value = self.value << 1 | (value >> shift) & 1
            self.count += 1
            if self.count == 8:
                self.out.append(self.value)
                self.value = self.count = 0

    def finish(self):
        if self.count:
            self.out.append(self.value << (8 - self.count))
            self.value = self.count = 0
        return bytes(self.out)


def rect(xmin, xmax, ymin, ymax):
    bits = max(max(abs(v) for v in (xmin, xmax, ymin, ymax)).bit_length() + 1, 1)
    packed = Bits()
    packed.put(bits, 5)
    for value in (xmin, xmax, ymin, ymax):
        packed.put(value, bits)
    return packed.finish()


def identity_matrix():
    packed = Bits()
    packed.put(0, 1)
    packed.put(0, 1)
    packed.put(0, 5)
    return packed.finish()


def tag(code, body=b"", long=False):
    if len(body) < 0x3F and not long:
        return struct.pack("<H", code << 6 | len(body)) + body
    return struct.pack("<HI", code << 6 | 0x3F, len(body)) + body


def string(text):
    return text.encode("utf-8") + b"\0"


EMPTY_SHAPE = b"\x10\x00"


def define_font_2(code, font_id, name, characters, bold=False):
    """A DefineFont2 or DefineFont3 of empty glyphs for `characters`, its codes two bytes."""
    count = len(characters)
    flags = 0x04 | (0x01 if bold else 0)
    table = 2 * count + 2
    offsets = b"".join(struct.pack("<H", table + 2 * i) for i in range(count))
    code_table_offset = struct.pack("<H", table + 2 * count)
    codes = b"".join(struct.pack("<H", ord(c)) for c in characters)
    body = (
        struct.pack("<HBBB", font_id, flags, 0, len(name))
        + name.encode("ascii")
        + struct.pack("<H", count)
        + offsets
        + code_table_offset
        + EMPTY_SHAPE * count
        + codes
    )
    return tag(code, body)


def define_font(font_id, characters):
    """A DefineFont of empty glyphs and the DefineFontInfo2 of its codes."""
    count = len(characters)
    offsets = b"".join(struct.pack("<H", 2 * count + 2 * i) for i in range(count))
    font = tag(DEFINE_FONT, struct.pack("<H", font_id) + offsets + EMPTY_SHAPE * count)
    name = b"Times"
    info = (
        struct.pack("<HB", font_id, len(name))
        + name
        + bytes([0x01, 1])
        + b"".join(struct.pack("<H", ord(c)) for c in characters)
    )
    return font + tag(DEFINE_FONT_INFO_2, info)


GLYPH_BITS = 7
ADVANCE_BITS = 11
ADVANCE = 120


def define_text(code, text_id, fonts, records):
    """A DefineText or DefineText2 of `records`, each (font, height, x, y, text), any of the
    first four None to keep what the record before set."""
    color_bytes = 4 if code == DEFINE_TEXT_2 else 3
    body = bytearray(struct.pack("<H", text_id))
    body += rect(0, 6000, 0, 2000)
    body += identity_matrix()
    body += bytes([GLYPH_BITS, ADVANCE_BITS])
    current = None
    for font, height, x, y, text in records:
        flags = 0x80
        flags |= 0x08 if font is not None else 0
        flags |= 0x04
        flags |= 0x02 if y is not None else 0
        flags |= 0x01 if x is not None else 0
        body.append(flags)
        if font is not None:
            body += struct.pack("<H", font)
            current = font
        body += b"\x00\x00\x00\xff"[:color_bytes]
        if x is not None:
            body += struct.pack("<h", x)
        if y is not None:
            body += struct.pack("<h", y)
        if font is not None:
            body += struct.pack("<H", height)
        body.append(len(text))
        glyphs = Bits()
        for c in text:
            glyphs.put(fonts[current].index(c), GLYPH_BITS)
            glyphs.put(ADVANCE, ADVANCE_BITS)
        body += glyphs.finish()
    body.append(0)
    return tag(code, bytes(body))


def define_edit_text(text_id, text, html=False, password=False, font=None):
    flags = 0x80 | 0x40 | 0x20 | 0x08 | 0x04
    flags |= 0x10 if password else 0
    flags |= 0x01 if font is not None else 0
    more = 0x20 | (0x02 if html else 0)
    body = struct.pack("<H", text_id) + rect(0, 6000, 0, 3000) + bytes([flags, more])
    if font is not None:
        body += struct.pack("<HH", font, 240)
    body += b"\x00\x00\x00\xff"
    body += struct.pack("<BHHHh", 0, 40, 40, 0, 40)
    body += string("") + string(text)
    return tag(DEFINE_EDIT_TEXT, body)


def grey_jpeg():
    """An 8 by 8 grey JPEG: its one block's DC difference and end of block coded with
    one-symbol tables."""

    def segment(marker, body):
        return bytes([0xFF, marker]) + struct.pack(">H", len(body) + 2) + body

    def huffman(table_class):
        return bytes([table_class, 1] + [0] * 15 + [0])

    return (
        b"\xff\xd8"
        + segment(0xDB, b"\x00" + b"\x01" * 64)
        + segment(0xC0, bytes([8, 0, 8, 0, 8, 1, 1, 0x11, 0]))
        + segment(0xC4, huffman(0x00))
        + segment(0xC4, huffman(0x10))
        + segment(0xDA, bytes([1, 1, 0, 0, 63, 0]))
        + b"\x3f"
        + b"\xff\xd9"
    )


def silent_mp3(frames):
    """`frames` MPEG-1 Layer III frames of silence, 128 kbit/s at 44.1 kHz, mono."""
    header = bytes([0xFF, 0xFB, 0x90, 0xC4])
    return (header + b"\0" * (417 - len(header))) * frames


def place_object(character, depth):
    return tag(PLACE_OBJECT_2, struct.pack("<BHH", 0x02, depth, character))


def characters(*texts):
    """The characters of `texts`, each once, in code order, as a font embeds them."""
    return "".join(sorted(set("".join(texts))))


def movie_body():
    title = "Skagen Lighthouse"
    lines = ["Lit every evening", " at dusk", "Tide", "06:12"]
    harbour = "Fyret lyser om natten over N\u00f8rre Kaj."
    fonts = {1: characters(title), 2: characters(*lines), 3: characters(harbour)}
    tags = [
        tag(FILE_ATTRIBUTES, struct.pack("<I", 0x18)),
        tag(METADATA, string(XMP)),
        tag(SET_BACKGROUND_COLOR, b"\xff\xff\xff"),
        define_font_2(DEFINE_FONT_3, 1, "Arial Bold", fonts[1], bold=True),
        define_font_2(DEFINE_FONT_2, 2, "Arial", fonts[2]),
        define_font(3, fonts[3]),
        define_text(
            DEFINE_TEXT,
            10,
            fonts,
            [
                (1, 480, 0, 400, title),
                (2, 240, 0, 900, lines[0]),
                (2, 240, None, None, lines[1]),
                (None, None, 0, 1300, lines[2]),
                (None, None, len(lines[2]) * ADVANCE + 200, None, lines[3]),
            ],
        ),
        define_text(
            DEFINE_TEXT_2,
            11,
            fonts,
            [(3, 240, 0, 300, harbour)],
        ),
        define_edit_text(12, HTML, html=True),
        define_edit_text(13, "Visitors welcome\rClosed on Sundays", font=2),
        define_edit_text(14, "lamp-room-key", password=True),
        tag(DEFINE_BITS_JPEG_2, struct.pack("<H", 20) + b"\xff\xd9\xff\xd8" + grey_jpeg()),
        tag(DEFINE_SOUND, struct.pack("<HBI", 21, 0x2E, 2 * 1152) + b"\0\0" + silent_mp3(2)),
        tag(
            DEFINE_SPRITE,
            struct.pack("<HH", 30, 1)
            + place_object(10, 1)
            + tag(SHOW_FRAME)
            + tag(END),
        ),
        place_object(30, 1),
        place_object(11, 2),
        place_object(12, 3),
        place_object(13, 4),
        tag(FRAME_LABEL, string("harbour")),
        tag(SHOW_FRAME),
        tag(END),
    ]
    header = rect(0, 11000, 0, 8000) + struct.pack("<HH", 24 << 8, 1)
    return header + b"".join(tags)


def main():
    out = sys.argv[1]
    body = movie_body()
    length = struct.pack("<I", 8 + len(body))
    version = bytes([VERSION])
    with open(os.path.join(out, "lighthouse-uncompressed.swf"), "wb") as f:
        f.write(b"FWS" + version + length + body)
    with open(os.path.join(out, "lighthouse.swf"), "wb") as f:
        f.write(b"CWS" + version + length + zlib.compress(body, 9))
    alone = lzma.compress(body, format=lzma.FORMAT_ALONE)
    properties, data = alone[:5], alone[13:]
    with open(os.path.join(out, "lighthouse-lzma.swf"), "wb") as f:
        f.write(b"ZWS" + version + length + struct.pack("<I", len(data)) + properties + data)


if __name__ == "__main__":
    main()
