"""Writes test_files/documents/lighthouse.dvi, a DVI file of two pages as TeX writes one, with
what DviParser reads and what it leaves out:

- the preamble TeX writes, its comment the date it ran;
- a heading in cmbx12, lines of cmr10 with an interword space through w and x and the lines
  moved down through y and z, a kern, the fi ligature, TeX's quotes and an en dash, and an
  acute accent TeX's \\accent sets before its letter;
- a line of cmti10 with the pound sign italic fonts set where cmr10 sets the dollar;
- an itemize label, the bullet of cmsy10, in a box set back to the margin;
- E = mc^2 in cmmi10, its superscript in cmr7 in a box shifted up;
- on the second page, a line of ecrm1000 in the T1 encoding, a line of cmtt10 with an
  underscore and braces, a line with the summation sign of cmex10, whose encoding DviParser
  does not know, and a \\special;
- each page's number centred at its foot, and the postamble with its font definitions.

The character widths, which TeX takes from the fonts' metric files, are those of Computer
Modern's for the characters of the accent; elsewhere the moves are TeX's own spaces and kerns.

Usage: lighthouse.py OUTPUT.dvi
"""

import struct
import sys

PRE, POST, POST_POST = 247, 248, 249
BOP, EOP, PUSH, POP = 139, 140, 141, 142
SET1, RIGHT4, DOWN4 = 128, 146, 160
W0, W3, X0, X3, Y0, Y3, Z0, Z3 = 147, 150, 152, 155, 161, 164, 166, 169
FNT_NUM_0, XXX1, FNT_DEF1 = 171, 239, 243

NUM, DEN, MAG = 25400000, 473628672, 1000
PT = 65536
COMMENT = b" TeX output 2026.11.13:1200"

# Font number, name and size, as TeX numbers them in the order it first uses them. Their
# checksums are written as 0, which asks no driver to check them against the metric files.
FONTS = [
    (0, b"cmbx12", 12 * PT),
    (1, b"cmr10", 10 * PT),
    (2, b"cmti10", 10 * PT),
    (3, b"cmsy10", 10 * PT),
    (4, b"cmmi10", 10 * PT),
    (5, b"cmr7", 7 * PT),
    (6, b"ecrm1000", 10 * PT),
    (7, b"cmtt10", 10 * PT),
    (8, b"cmex10", 10 * PT),
]
SPACE = 218453
EM = 10 * PT


def signed(opcode, value):
    return bytes([opcode]) + struct.pack(">i", value)


def right(by):
    return signed(RIGHT4, by)


def down(by):
    return signed(DOWN4, by)


DEFINED = set()


def font(number):
    """Selects the font `number`, defined first where it is first used, as TeX defines it."""
    definition = b""
    if number not in DEFINED:
        DEFINED.add(number)
        definition = font_definition(*FONTS[number])
    return definition + bytes([FNT_NUM_0 + number])


def text(characters):
    """Sets `characters`, by their Latin-1 codes: by set_char below 128, else by set1."""
    return b"".join(
        bytes([code]) if code < 128 else bytes([SET1, code])
        for code in characters.encode("latin-1")
    )


def words(*runs):
    """Sets the `runs` with TeX's interword space between them, the first through w3 and the
    rest through w0."""
    out = text(runs[0])
    for index, run in enumerate(runs[1:]):
        out += (bytes([W3]) + struct.pack(">i", SPACE)[1:]) if index == 0 else bytes([W0])
        out += text(run)
    return out


def line(*commands):
    return bytes([PUSH]) + b"".join(commands) + bytes([POP])


def font_definition(number, name, size):
    return (
        bytes([FNT_DEF1, number])
        + struct.pack(">III", 0, size, size)
        + bytes([0, len(name)])
        + name
    )


def page_one():
    acute = right(-18204) + bytes([0x13]) + right(-(EM // 2 - 18204))
    return b"".join(
        [
            down(40 * PT),
            line(font(0), words("The", "Skagen", "Light")),
            bytes([Y3]) + struct.pack(">i", 24 * PT)[1:],
            line(
                font(1),
                words("The", "lamp", "was"),
                bytes([X3]) + struct.pack(">i", 230000)[1:],
                bytes([0x0C]),
                text("rst"),
                bytes([X0]),
                words("lit", "in", "1858."),
                bytes([X0]),
                text("W"),
                right(-54613),
                words("ardens", "kept", "a", "log."),
            ),
            bytes([Z3]) + struct.pack(">i", 12 * PT)[1:],
            line(
                font(1),
                words("He", "wrote", "of", "the", "caf"),
                acute,
                text("e"),
                bytes([W0]),
                words("and", "of"),
                bytes([W0]),
                bytes([0x5C]),
                text("calm"),
                bytes([0x22]),
                bytes([W0]),
                words("seas,", "1858"),
                bytes([0x7B]),
                text("1958."),
            ),
            bytes([Z0]),
            line(font(2), words("The", "oil", "cost", "$4", "a", "barrel.")),
            bytes([Z0]),
            line(
                bytes([PUSH]),
                right(15 * PT),
                font(3),
                bytes([0x0F]),
                bytes([POP]),
                right(25 * PT),
                font(1),
                words("Trim", "the", "wick."),
            ),
            bytes([Z0]),
            line(
                right(150 * PT),
                font(4),
                text("E"),
                right(181817),
                font(1),
                text("="),
                right(181817),
                font(4),
                text("mc"),
                bytes([PUSH]),
                down(-237825),
                font(5),
                text("2"),
                bytes([POP]),
            ),
            bytes([Y0]),
            line(right(160 * PT), font(1), text("1")),
        ]
    )


def page_two():
    special = b"color push gray 0"
    return b"".join(
        [
            down(40 * PT),
            line(font(6), words("Fyret", "p\xe5", "Skagen"), right(-11000), text("s")),
            bytes([Z3]) + struct.pack(">i", 12 * PT)[1:],
            line(font(7), words("log_book.txt", "{1858}")),
            bytes([Z0]),
            line(
                font(1),
                words("Total:"),
                right(SPACE),
                bytes([XXX1, len(special)]) + special,
                font(8),
                bytes([0x50]),
                right(166666),
                font(4),
                text("n"),
            ),
            bytes([Z0]),
            line(right(160 * PT), font(1), text("2")),
        ]
    )


def main():
    out = bytearray(bytes([PRE, 2]) + struct.pack(">III", NUM, DEN, MAG))
    out += bytes([len(COMMENT)]) + COMMENT
    previous = -1
    for number, body in enumerate([page_one(), page_two()], start=1):
        start = len(out)
        counters = struct.pack(">10i", number, *[0] * 9)
        out += bytes([BOP]) + counters + struct.pack(">i", previous) + body + bytes([EOP])
        previous = start
    post = len(out)
    out += bytes([POST]) + struct.pack(">iIIIIIHH", previous, NUM, DEN, MAG, 50 * PT, 30 * PT, 2, 2)
    out += b"".join(font_definition(*definition) for definition in FONTS)
    out += bytes([POST_POST]) + struct.pack(">i", post) + bytes([2])
    out += bytes([223]) * (4 + (-(len(out) + 4)) % 4)
    with open(sys.argv[1], "wb") as output:
        output.write(out)


if __name__ == "__main__":
    main()
