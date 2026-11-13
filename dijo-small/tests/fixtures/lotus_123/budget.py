"""Writes test_files/documents/lotus-r9.123, a workbook of Lotus 1-2-3 97 or Millennium (release
9.x), its records laid out as the .123 files in LibreOffice's and Tika's test documents lay theirs
out, with what Lotus123Parser reads and what it leaves out:

- a BOF record of version 0x1003, which Tika's magic detects as 1-2-3 97 and 9.x, its text in
  LMBCS group 1, and a sheet named "Budget";
- labels, doubles, a formula's double result, and an NA and an ERR cell;
- the formats database between its two FORMATS records: a PACKINFO record of 16-bit counts,
  NODE runs by sheet, column and row in BEGIN_GROUP and END_GROUP records, DATANODE formats of
  currency, percent (one in parentheses), a date, an extended date of the fixed type and a time
  of day, and an empty column registered once and used again for the columns and sheets left;
- a column widths database after it, whose runs hold no formats;
- the comment of a cell;
- after the EOF record, the document information 1-2-3 writes there, its author and revisions.

The formula's code is its end alone: the parser writes the result the file keeps.

Usage: budget.py OUTPUT.123
"""

import struct
import sys

BOF = 0x0000
EOF = 0x0001
ERROR = 0x0014
NOT_AVAILABLE = 0x0015
LABEL = 0x0016
NAMED_SHEET = 0x0023
CELL_COMMENT = 0x0026
DOUBLE_NUMBER = 0x0027
DOUBLE_FORMULA = 0x0028
BEGIN_GROUP = 0x0106
END_GROUP = 0x0107
FORMATS = 0x0293
COLUMN_WIDTHS = 0x0295
RL2DB = 0x0299
RL3DB = 0x029A
NODE = 0x0800
DATANODE = 0x0801
REGISTER_ID = 0x0802
USE_REGISTERED_ID = 0x0803
PACKINFO = 0x0804

ROWS = 8192
COLUMNS = 256
SHEETS = 256
EMPTY_COLUMN = 1


def record(kind, body=b""):
    return struct.pack("<HH", kind, len(body)) + body


def cell(kind, row, column, payload):
    return record(kind, struct.pack("<HBB", row, 0, column) + payload)


def label(row, column, text):
    return cell(LABEL, row, column, b"'" + text.encode("ascii") + b"\0")


def number(row, column, value):
    return cell(DOUBLE_NUMBER, row, column, struct.pack("<d", value))


def node(length):
    return record(NODE, struct.pack("<H", length))


def data(code, attributes=0, extension=0, flags=0):
    return record(DATANODE, bytes([code, attributes, extension, flags]))


def group(*records):
    return record(BEGIN_GROUP) + b"".join(records) + record(END_GROUP)


def column(*rows):
    """A run of one column of `rows`, each a length and the format of its run, if any."""
    runs = []
    for length, format_record in rows:
        runs.append(node(length))
        if format_record is not None:
            runs.append(format_record)
    return node(1) + group(*runs)


def formats():
    sheet = group(
        node(1)
        + group(record(REGISTER_ID, struct.pack("<H", EMPTY_COLUMN)), node(ROWS)),
        column((1, None), (3, data(0x22)), (ROWS - 4, None)),
        column(
            (1, None),
            (1, data(0x31)),
            (1, data(0x31, attributes=0x02)),
            (1, data(0x31)),
            (ROWS - 4, None),
        ),
        column(
            (1, None),
            (1, data(0x72)),
            (1, data(0x02, extension=0x63, flags=0x80)),
            (ROWS - 3, None),
        ),
        column((1, None), (1, data(0x7C)), (ROWS - 2, None)),
        node(COLUMNS - 5),
        record(USE_REGISTERED_ID, struct.pack("<H", EMPTY_COLUMN)),
    )
    others = group(
        node(COLUMNS), record(USE_REGISTERED_ID, struct.pack("<H", EMPTY_COLUMN))
    )
    return (
        record(FORMATS)
        + record(RL3DB, bytes(10))
        + record(PACKINFO, struct.pack("<HHHHH", 3, 3, ROWS, COLUMNS, SHEETS))
        + group(node(1), sheet, node(SHEETS - 1), others)
        + record(FORMATS)
    )


def column_widths():
    return (
        record(COLUMN_WIDTHS)
        + record(RL2DB, bytes(10))
        + record(PACKINFO, struct.pack("<HHHH", 3, 2, COLUMNS, SHEETS))
        + group(node(SHEETS), group(node(COLUMNS), record(DATANODE, bytes(12))))
        + record(COLUMN_WIDTHS)
    )


def cells():
    return b"".join(
        [
            label(0, 0, "Item"),
            label(0, 1, "Cost"),
            label(0, 2, "Share"),
            label(0, 3, "Due"),
            label(0, 4, "Time"),
            label(1, 0, "Lamp oil"),
            number(1, 1, 1234.5),
            number(1, 2, 0.25),
            number(1, 3, 45000.0),
            number(1, 4, 0.75),
            label(2, 0, "Wicks"),
            number(2, 1, -12.0),
            number(2, 2, 0.125),
            number(2, 3, 45031.0),
            cell(NOT_AVAILABLE, 2, 4, b""),
            label(3, 0, "Total"),
            cell(DOUBLE_FORMULA, 3, 1, struct.pack("<d", 1222.5) + b"\x03"),
            cell(ERROR, 3, 2, b""),
            cell(CELL_COMMENT, 3, 0, b"\x01Checked by the keeper\0"),
        ]
    )


def document_information():
    return (
        record(0x0185, b"Keeper")
        + record(0x0189, b"Keeper")
        + record(0x018C, struct.pack("<H", 2))
    )


def workbook():
    bof = struct.pack("<HH", 0x1003, 0x0004)
    bof += bytes([0, 0, 0, 0, 3, 0, 0, 4, 0, 0, 0, 0, 1, 10])
    bof += bytes(26 - len(bof))
    return (
        record(BOF, bof)
        + record(NAMED_SHEET, struct.pack("<HH", 14000, 0) + b"Budget\0")
        + formats()
        + column_widths()
        + cells()
        + record(EOF)
        + document_information()
    )


if __name__ == "__main__":
    with open(sys.argv[1], "wb") as output:
        output.write(workbook())
