"""Writes test_files/documents/pier.dxf and pier-binary.dxf: one AutoCAD 2000 (AC1015) drawing
as an ASCII DXF, its lines ended as AutoCAD on Windows ends them, and as a binary DXF, with what
DxfParser reads and what it leaves out:

- the header's $ACADVER, $DWGCODEPAGE (ANSI_1252, in which the strings of the drawing are
  written) and the UTC dates the drawing was created and last updated, $TDUCREATE and $TDUUPDATE,
  as Julian days;
- a block of a TEXT and an ATTDEF, whose prompt is left out, and the anonymous block *D1 the
  dimension is drawn with, whose MTEXT repeats the dimension's text and is left out;
- TEXT entities with the control codes %%c, %%d and %%u, a literal percent sign, a character of
  code page 1252 and a \\U+ escape of one outside it;
- an MTEXT of more than 250 characters, in group 3 chunks and a last group 1, with fonts, a
  stack, underlining, a height, a tab written as ^I, an escaped backslash and paragraph breaks;
- an INSERT of the block with its ATTRIB;
- a linear DIMENSION whose text, "<> m", puts its measurement in place of the "<>";
- the DWGPROPS record of the named object dictionary: the drawing properties AutoCAD's
  DWGPROPS command sets, a custom property among them.

The drawing is written from the DXF reference, not saved by AutoCAD: it has no TABLES or
CLASSES section, which DxfParser does not read. ezdxf reads both files to the same entities and
properties:

    nix shell --impure --expr '(import <nixpkgs> {}).python3.withPackages (p: [ p.ezdxf ])' \
        -c python3 -c 'import ezdxf, sys; d = ezdxf.readfile(sys.argv[1]); \
            print([e.dxftype() for e in d.modelspace()])' pier.dxf

Usage: pier.py OUTPUT-DIRECTORY
"""

import os
import struct
import sys
from datetime import datetime, timezone

CODE_PAGE = "cp1252"
BINARY_SENTINEL = b"AutoCAD Binary DXF\r\n\x1a\x00"
UNIX_EPOCH_JULIAN_DAY = 2440587.5


def julian_day(text):
    """The Julian day of the UTC date-time `text`, as AutoCAD writes a date."""
    instant = datetime.fromisoformat(text).replace(tzinfo=timezone.utc)
    return UNIX_EPOCH_JULIAN_DAY + instant.timestamp() / 86400


MTEXT = (
    "{\\fArial|b1|i0|c0|p34;Mooring notes}\\P"
    "Mooring lines are checked at every change of the tide, and doubled when the wind is "
    "forecast above force 6. Fenders hang between every pair of bollards; replace any that "
    "are split. Keep \\S1/2; a fathom of slack on the bow lines at high water.\\P"
    "\\LNo vessel\\l may lie at the berth overnight without the leave of the harbour master. "
    "{\\H1.5x;Depth^I4 m} at the berth head, drawing P4\\\\B."
)


def chunks(text, size=250):
    """The groups of an MTEXT's `text`: chunks of `size` characters in group 3, the rest in 1."""
    pieces = [text[at : at + size] for at in range(0, len(text), size)]
    return [(3, piece) for piece in pieces[:-1]] + [(1, pieces[-1])]


def entity(kind, handle, owner, subclass, layer="0"):
    return [(0, kind), (5, handle), (330, owner), (100, "AcDbEntity"), (8, layer), (100, subclass)]


def point(code, x, y, z=0.0):
    return [(code, x), (code + 10, y), (code + 20, z)]


def text(handle, owner, value, x, y):
    return (
        entity("TEXT", handle, owner, "AcDbText")
        + point(10, x, y)
        + [(40, 2.5), (1, value), (100, "AcDbText")]
    )


def header():
    return [
        (0, "SECTION"),
        (2, "HEADER"),
        (9, "$ACADVER"),
        (1, "AC1015"),
        (9, "$DWGCODEPAGE"),
        (3, "ANSI_1252"),
        (9, "$INSBASE"),
        *point(10, 0.0, 0.0),
        (9, "$TDUCREATE"),
        (40, julian_day("2026-11-02T08:30:00")),
        (9, "$TDUUPDATE"),
        (40, julian_day("2026-11-12T21:04:05")),
        (0, "ENDSEC"),
    ]


def block(handle, name, flags, content, end):
    return (
        entity("BLOCK", handle, "1F", "AcDbBlockBegin")
        + [(2, name), (70, flags)]
        + point(10, 0.0, 0.0)
        + [(3, name), (1, "")]
        + content
        + entity("ENDBLK", end, "1F", "AcDbBlockEnd")
    )


def blocks():
    bollard = text("31", "30", "Bollard", 0.0, 3.0) + (
        entity("ATTDEF", "32", "30", "AcDbText")
        + point(10, 0.0, -1.0)
        + [(40, 2.5), (1, "B-1"), (100, "AcDbAttributeDefinition")]
        + [(3, "Bollard number"), (2, "NUMBER"), (70, 0)]
    )
    dimension = entity("MTEXT", "41", "40", "AcDbMText") + point(10, 6.25, 1.0) + [
        (40, 2.5),
        (41, 0.0),
        (71, 5),
        (1, "12.5 m"),
    ]
    return (
        [(0, "SECTION"), (2, "BLOCKS")]
        + block("20", "*Model_Space", 0, [], "21")
        + block("30", "BOLLARD", 2, bollard, "33")
        + block("40", "*D1", 1, dimension, "42")
        + [(0, "ENDSEC")]
    )


def entities():
    mtext = (
        entity("MTEXT", "55", "1F", "AcDbMText")
        + point(10, 0.0, -20.0)
        + [(40, 2.5), (41, 120.0), (71, 1), (72, 5)]
        + chunks(MTEXT)
        + [(7, "Standard")]
    )
    insert = (
        entity("INSERT", "56", "1F", "AcDbBlockReference")
        + [(66, 1), (2, "BOLLARD")]
        + point(10, 40.0, 0.0)
        + entity("ATTRIB", "57", "56", "AcDbText")
        + point(10, 40.0, -1.0)
        + [(40, 2.5), (1, "B-7"), (100, "AcDbAttribute"), (2, "NUMBER"), (70, 0)]
        + entity("SEQEND", "58", "56", "AcDbEntity")[:-1]
    )
    dimension = (
        entity("DIMENSION", "59", "1F", "AcDbDimension")
        + [(2, "*D1")]
        + point(10, 12.5, 2.0)
        + point(11, 6.25, 3.0)
        + [(70, 32), (1, "<> m"), (3, "Standard"), (42, 12.5)]
        + [(100, "AcDbAlignedDimension")]
        + point(13, 0.0, 0.0)
        + point(14, 12.5, 0.0)
        + [(100, "AcDbRotatedDimension")]
    )
    return (
        [(0, "SECTION"), (2, "ENTITIES")]
        + text("50", "1F", "PIER 4", 0.0, 10.0)
        + text("51", "1F", "%%c600 mm piles at 50% spacing", 0.0, 6.0)
        + text("52", "1F", "N\u00f8rre Kaj, bearing 45%%d", 0.0, 2.0)
        + text("53", "1F", "Tide gauge \\U+2192 harbour office", 0.0, -2.0)
        + text("54", "1F", "%%uLIFEBUOY%%u", 0.0, -6.0)
        + mtext
        + insert
        + dimension
        + [(0, "ENDSEC")]
    )


def objects():
    custom = [(300, "Harbour=Skagen")] + [(code, "=") for code in range(301, 310)]
    return [
        (0, "SECTION"),
        (2, "OBJECTS"),
        (0, "DICTIONARY"),
        (5, "C"),
        (330, "0"),
        (100, "AcDbDictionary"),
        (281, 1),
        (3, "DWGPROPS"),
        (350, "D0"),
        (0, "XRECORD"),
        (5, "D0"),
        (102, "{ACAD_REACTORS"),
        (330, "C"),
        (102, "}"),
        (330, "C"),
        (100, "AcDbXrecord"),
        (280, 1),
        (1, "DWGPROPS COOKIE"),
        (2, "Pier 4 Mooring Plan"),
        (3, "Moorings at the north pier"),
        (4, "Anna Berg"),
        (6, "Surveyed at low water"),
        (7, "pier, moorings, bollards"),
        (8, "Ove Gr\u00f8n"),
        (9, "3"),
        *custom,
        (40, 0.0),
        (41, julian_day("2026-11-02T08:30:00")),
        (42, julian_day("2026-11-12T21:04:05")),
        (1, "https://example.org/harbour/pier-4"),
        (90, 0),
        (0, "ENDSEC"),
    ]


def groups():
    return header() + blocks() + entities() + objects() + [(0, "EOF")]


def ascii_dxf(groups):
    lines = []
    for code, value in groups:
        lines.append(f"{code:>3}")
        lines.append(repr(value) if isinstance(value, float) else str(value))
    return ("\r\n".join(lines) + "\r\n").encode(CODE_PAGE)


def binary_value(code, value):
    """`value` of group `code` as a binary DXF writes it."""
    if code <= 9 or 100 <= code <= 109 or 300 <= code <= 309 or 320 <= code <= 369:
        return value.encode(CODE_PAGE) + b"\x00"
    if 10 <= code <= 59:
        return struct.pack("<d", value)
    if 60 <= code <= 79 or 270 <= code <= 289:
        return struct.pack("<h", value)
    if 90 <= code <= 99:
        return struct.pack("<i", value)
    raise ValueError(f"group code {code} is not written by this script")


def binary_dxf(groups):
    return BINARY_SENTINEL + b"".join(
        struct.pack("<H", code) + binary_value(code, value) for code, value in groups
    )


if __name__ == "__main__":
    directory = sys.argv[1]
    drawing = groups()
    with open(os.path.join(directory, "pier.dxf"), "wb") as output:
        output.write(ascii_dxf(drawing))
    with open(os.path.join(directory, "pier-binary.dxf"), "wb") as output:
        output.write(binary_dxf(drawing))
