"""Writes a Lotus Notes database (.nsf) of ODS 43, as Notes 6 and 7 write them, from the structures
public documentation describes, not from Notes: Joachim Metz's "Notes Storage Facility (NSF)
database file format" and libnsfdb (https://github.com/libyal/libnsfdb), the Notes C API reference
(https://opensource.hcltechsw.com/domino-c-api-docs/) for item values and CD records, and the
layouts observed in Stocks.nsf of Apache OpenOffice (main/odk/examples/java/NotesAccess,
Apache-2.0), which Notes 5 wrote. It holds what LotusNotesParser reads and what it leaves out:

- the database header, of the title "Lighthouse Log";
- a superblock and a bucket descriptor block, both CX-compressed with literals and back
  references, the superblock listing one summary bucket and one non-summary bucket, the bucket
  descriptor block a data and a non-data RRV bucket and the unique name key table;
- three data notes: a log entry of text, a text list, a time, numbers, a rich-text Body of
  paragraphs, a line break and a table, kept in the non-summary bucket, and a $FILE attachment
  whose object lies outside the buckets; a second entry of a number range, a time range of an
  all-day date and a pair of times, a one-paragraph Body, and system items whose names start
  with $; and a deletion stub;
- a form note, a design note in the non-data RRV bucket, which is not written.

The variants write what the parser declines: "sealed" a log entry encrypted for its readers, with
$Seal and $SealData items; "lz1-attachment" the attachment compressed with LZ1.

Usage: log.py log|sealed|lz1-attachment OUTPUT.nsf
"""

import struct
import sys

ODS_VERSION = 43
DATABASE_HEADER_SIZE = 1024
RRV_BUCKET_SIZE = 4096
RRV_ENTRIES = (RRV_BUCKET_SIZE - 32) // 8
SUMMARY_BUCKET_SIZE = 8192
NON_SUMMARY_BUCKET_SIZE = 16384
BUCKET_HEADER_SIZE = 66
BUCKET_FOOTER_SIZE = 8
SUMMARY_PAGE_SIZE = 8206
NON_SUMMARY_PAGE_SIZE = 8198

ITEM_SUMMARY = 0x0004
ITEM_NAMES = 0x0040
ITEM_SEAL = 0x0002

TYPE_COMPOSITE = 0x0001
TYPE_OBJECT = 0x0003
TYPE_SEAL = 0x0009
TYPE_SEALDATA = 0x000A
TYPE_NUMBER = 0x0300
TYPE_NUMBER_RANGE = 0x0301
TYPE_TIME = 0x0400
TYPE_TIME_RANGE = 0x0401
TYPE_TEXT = 0x0500
TYPE_TEXT_LIST = 0x0501

SIG_CD_PARAGRAPH = 0x81
SIG_CD_PABDEFINITION = 0x82
SIG_CD_PABREFERENCE = 0x83
SIG_CD_TEXT = 0x85
SIG_CD_TABLEBEGIN = 0xA3
SIG_CD_TABLECELL = 0xA4
SIG_CD_TABLEEND = 0xA5

COMPRESS_NONE = 0
COMPRESS_LZ1 = 2

DATA_RRV_START = 0x0904
NON_DATA_RRV_START = 0x0106

CREATED = (0x00672189, 0xC1256AC6)


def julian_day(year, month, day):
    a = (14 - month) // 12
    y = year + 4800 - a
    m = month + 12 * a - 3
    return day + (153 * m + 2) // 5 + 365 * y + y // 4 - y // 100 + y // 400 - 32045


def timedate(year, month, day, hour=None, minute=0, second=0):
    """A TIMEDATE in GMT; without an hour, an all-day date."""
    ticks = 0xFFFFFFFF if hour is None else ((hour * 60 + minute) * 60 + second) * 100
    return struct.pack("<II", ticks, julian_day(year, month, day))


def time_of_day(hour, minute):
    """A TIMEDATE of a time of any day."""
    return struct.pack("<II", (hour * 60 + minute) * 6000, 0xFFFFFFFF)


def pad(data, size):
    assert len(data) <= size, (len(data), size)
    return data + b"\0" * (size - len(data))


def xor32(data):
    checksum = 0
    for at in range(0, len(data), 4):
        checksum ^= struct.unpack("<I", pad(data[at : at + 4], 4))[0]
    return checksum


class Bits:
    """Bits written from the low bit of each byte up."""

    def __init__(self):
        self.bits = []

    def put(self, value, count):
        self.bits.extend((value >> at) & 1 for at in range(count))

    def unary(self, count):
        self.put(1 << (count - 1), count)

    def bytes(self):
        padded = self.bits + [0] * (-len(self.bits) % 8)
        return bytes(
            sum(bit << at for at, bit in enumerate(padded[start : start + 8]))
            for start in range(0, len(padded), 8)
        )


def cx_compress(data):
    """`data` CX-compressed: three header bits of 4, then literals and back references found
    greedily, ended by a reference of length 0x102."""
    bits = Bits()
    bits.put(4, 3)
    at = 0
    while at < len(data):
        length, offset = 0, 0
        for back in range(1, min(at, 4095) + 1):
            run = 0
            while at + run < len(data) and run < 257 and data[at + run - back] == data[at + run]:
                run += 1
            if run > length and (run > 2 or (run == 2 and back < 256)):
                length, offset = run, back
        if length == 0:
            bits.put(0, 1)
            bits.put(data[at], 8)
            at += 1
            continue
        bits.put(1, 1)
        if length == 2:
            bits.put(1, 1)
        else:
            bits.put(0, 1)
            extra = length - 2
            count = (extra + 1).bit_length() - 1
            bits.unary(count)
            bits.put(extra - ((1 << count) - 1), count)
            high = offset >> 8
            if high == 0:
                bits.put(1, 1)
            else:
                bits.put(0, 1)
                count = (high + 1).bit_length() - 1
                bits.unary(count)
                bits.put(high - ((1 << count) - 1), count)
        bits.put(offset & 0xFF, 8)
        at += length
    bits.put(1, 1)
    bits.put(0, 1)
    bits.unary(8)
    bits.put(1, 8)
    return bits.bytes()


def compressed_block(data):
    stream = cx_compress(data)
    return struct.pack("<I", len(stream)) + stream


def lmbcs(text):
    """`text` in LMBCS of the default group 1, code page 850."""
    return b"".join(
        bytes([ord(c)]) if ord(c) < 0x80 else c.encode("cp850") for c in text
    )


def text_list(values):
    encoded = [lmbcs(value) for value in values]
    return (
        struct.pack("<H", len(encoded))
        + b"".join(struct.pack("<H", len(value)) for value in encoded)
        + b"".join(encoded)
    )


def number_range(values):
    return struct.pack("<HH", len(values), 0) + b"".join(struct.pack("<d", v) for v in values)


def time_range(times, pairs):
    return (
        struct.pack("<HH", len(times), len(pairs))
        + b"".join(times)
        + b"".join(lower + upper for lower, upper in pairs)
    )


def cd(signature, body, kind):
    """A CD record of `kind` byte, word or long length, padded to an even length."""
    if kind == "byte":
        record = bytes([signature, 2 + len(body)]) + body
    elif kind == "word":
        record = bytes([signature, 0xFF]) + struct.pack("<H", 4 + len(body)) + body
    else:
        record = bytes([signature, 0]) + struct.pack("<I", 6 + len(body)) + body
    return record + b"\0" * (len(record) % 2)


FONT = struct.pack("<I", 0x0A000000)


def paragraph(*runs):
    return (
        cd(SIG_CD_PARAGRAPH, b"", "byte")
        + cd(SIG_CD_PABREFERENCE, struct.pack("<H", 1), "byte")
        + b"".join(cd(SIG_CD_TEXT, FONT + lmbcs(run), "word") for run in runs)
    )


def table(rows):
    records = cd(SIG_CD_TABLEBEGIN, b"\0" * 14, "byte")
    for row, cells in enumerate(rows):
        for column, text in enumerate(cells):
            cell = struct.pack("<BBHHHBBHBBH", row, column, 0, 0, 0, 0, 0, 0, 1, 1, 0)
            records += cd(SIG_CD_TABLECELL, cell, "byte") + paragraph(text)
    return records + cd(SIG_CD_TABLEEND, b"\0" * 4, "byte")


def rich_text(*parts):
    return cd(SIG_CD_PABDEFINITION, b"\0" * 86, "word") + b"".join(parts)


TIDE = b"High water at 05:12, low water at 11:30.\n"


class Item:
    def __init__(self, name, kind, value, flags=ITEM_SUMMARY):
        self.name, self.kind, self.value, self.flags = name, kind, value, flags


def file_object(name, size, compression, object_id):
    encoded = lmbcs(name)
    return (
        struct.pack("<HHIHHHHHI", TYPE_OBJECT, 0, object_id, len(encoded), 0, compression, 0, 0, size)
        + timedate(2026, 3, 14, 6, 0)
        + timedate(2026, 3, 14, 6, 0)
        + encoded
    )


def entry(variant, object_id):
    body = rich_text(
        paragraph("The lamp was lit at dusk."),
        paragraph("Waves broke over the ", "north pier;\0no ships passed."),
        table([["Hour", "Wind"], ["21:00", "SW 9"]]),
        paragraph("Ove signed the log."),
    )
    compression = COMPRESS_LZ1 if variant == "lz1-attachment" else COMPRESS_NONE
    items = [
        Item("Form", TYPE_TEXT, lmbcs("Entry")),
        Item("Subject", TYPE_TEXT, lmbcs("Storm on the north pier")),
        Item("Keepers", TYPE_TEXT_LIST, text_list(["Anna Berg", "Ove Grøn"]), ITEM_SUMMARY | ITEM_NAMES),
        Item("Lit", TYPE_TIME, timedate(2026, 3, 14, 18, 5)),
        Item("Ships", TYPE_NUMBER, struct.pack("<d", 3.0)),
        Item("Wind", TYPE_NUMBER, struct.pack("<d", 12.5)),
        Item("Body", TYPE_COMPOSITE, body, 0),
        Item("$FILE", TYPE_OBJECT, file_object("tide.txt", len(TIDE), compression, object_id)),
        Item("$UpdatedBy", TYPE_TEXT_LIST, text_list(["Anna Berg"]), ITEM_SUMMARY | ITEM_NAMES),
    ]
    if variant == "sealed":
        items[6].flags |= ITEM_SEAL
        items += [
            Item("$Seal", TYPE_SEAL, b"\x01" * 24, 0),
            Item("$SealData", TYPE_SEALDATA, b"\x02" * 40, 0),
        ]
    return items


def calm():
    return [
        Item("Form", TYPE_TEXT, lmbcs("Entry")),
        Item("Subject", TYPE_TEXT, lmbcs("Calm night")),
        Item("Pressure", TYPE_NUMBER_RANGE, number_range([1013.0, 1015.5])),
        Item(
            "Watch",
            TYPE_TIME_RANGE,
            time_range([timedate(2026, 3, 15)], [(time_of_day(18, 0), time_of_day(6, 30))]),
        ),
        Item("Body", TYPE_COMPOSITE, rich_text(paragraph("No ships, a clear sky.")), 0),
        Item("$Revisions", TYPE_TIME_RANGE, time_range([timedate(2026, 3, 15, 7, 0)], [])),
    ]


def form():
    return [
        Item("$TITLE", TYPE_TEXT, lmbcs("Entry")),
        Item("$Body", TYPE_COMPOSITE, rich_text(paragraph("Subject:")), 0),
    ]


class Names:
    """The unique name key table: an entry per name and type."""

    def __init__(self):
        self.keys = []

    def index(self, name, kind):
        key = (name, kind)
        if key not in self.keys:
            self.keys.append(key)
        return self.keys.index(key)

    def table(self):
        text = b""
        entries = b""
        for name, kind in self.keys:
            encoded = lmbcs(name)
            entries += struct.pack("<IHBBH", len(text), len(encoded), kind & 0xFF, kind >> 8, 0)
            text += encoded
        return entries + struct.pack("<I", len(text)) + text, len(self.keys), len(text)


OID_FILE = struct.pack("<II", 0x0BADF00D, 0xC1256AC6)


def originator(rrv, sequence):
    return OID_FILE + struct.pack("<II", 0x10000 + rrv, 0xC1256AC7) + struct.pack("<I", sequence) + timedate(2026, 3, 15, 7, 0)


def note(rrv, note_class, items, names, non_summary_id):
    """The note `rrv` of `items` and the non-summary record of those not in its summary, if any."""
    descriptors = b""
    summary = b""
    non_summary = b""
    for item in items:
        descriptors += struct.pack("<HHHH", names.index(item.name, item.kind), item.flags, len(item.value), 0x0100)
        if item.flags & ITEM_SUMMARY:
            summary += item.value
        else:
            non_summary += item.value
    record = b""
    if non_summary:
        size = 64 + 4 + len(non_summary)
        header = (
            struct.pack("<HII", 0x0010, size, rrv)
            + originator(rrv, 1)
            + timedate(2026, 3, 15, 7, 0)
            + struct.pack("<I", size)
            + b"\0" * 12
            + struct.pack("<H", 1)
        )
        assert len(header) == 64
        record = header + b"\0" * 4 + non_summary
    size = 100 + len(descriptors) + len(summary)
    header = (
        struct.pack("<HII", 0x0004, size, rrv)
        + originator(rrv, 1)
        + struct.pack("<HH", 0x0002, note_class)
        + timedate(2026, 3, 15, 7, 0)
        + struct.pack("<HHHII", len(items), 0, 0, non_summary_id if record else 0, len(record))
        + timedate(2026, 3, 15, 7, 0)
        + timedate(2026, 3, 14, 18, 0)
        + struct.pack("<IHIIIH", 0, 3, 0, 3, 0, 0)
    )
    assert len(header) == 100
    return header + descriptors + summary, record


def stub(rrv):
    return pad(struct.pack("<HII", 0x0004, 64, rrv) + originator(rrv, 2) + struct.pack("<HH", 0x0083, 1), 64)


def bucket(slots, size):
    """A bucket of `slots`, its index from the back, its offsets from the bucket's start."""
    data = b""
    index = b""
    at = BUCKET_HEADER_SIZE + 2
    for slot in slots:
        index = struct.pack("<HH", at + len(data), len(slot)) + index
        data += slot + b"\0" * (len(slot) % 2)
    header = pad(
        struct.pack("<BBII", 0x02, 0x42, 0, size)
        + timedate(2026, 3, 15, 7, 0)
        + b"\0" * 20
        + struct.pack("<HIIHI", 1, 0, len(slots), 0, BUCKET_FOOTER_SIZE),
        BUCKET_HEADER_SIZE,
    )
    body = header + b"\0\0" + data
    return pad(body, size - BUCKET_FOOTER_SIZE - len(index)) + index + b"\0" * BUCKET_FOOTER_SIZE


def rrv_bucket(first, entries):
    """An RRV bucket of `entries` by RRV identifier from `first`: (bucket, slot) or a position."""
    table = b""
    for at in range(RRV_ENTRIES):
        target = entries.get(first + 4 * at)
        if target is None:
            table += struct.pack("<II", 0, 0)
        elif isinstance(target, tuple):
            table += struct.pack("<II", 0x80000000 | target[0], target[1])
        else:
            table += struct.pack("<II", target >> 8, 0)
    header = pad(struct.pack("<BBII", 0x06, 0x20, 0, first) + b"\0" * 8, 18) + struct.pack("<I", xor32(table))
    return pad(header, 32) + table


def superblock(summary_positions, non_summary_positions, write_count):
    summary_page = b"\0" * 224 + b"".join(
        struct.pack("<I", position >> 8) + timedate(2026, 3, 15, 7, 0) + b"\x10\x08" for position in summary_positions
    )
    non_summary_page = b"\0" * 70 + b"".join(
        struct.pack("<I", position >> 8) + b"\x20\x10" for position in non_summary_positions
    )
    data = pad(summary_page, SUMMARY_PAGE_SIZE) + pad(non_summary_page, NON_SUMMARY_PAGE_SIZE)
    body = compressed_block(data)
    stored = 100 + len(body) + 12
    header = (
        struct.pack("<H", 0x000E)
        + timedate(2026, 3, 15, 7, 0)
        + struct.pack("<IIIII", len(data), len(summary_positions), len(non_summary_positions), 0, RRV_BUCKET_SIZE)
        + b"\0" * 28
        + struct.pack("<HIIHII", 0, write_count, stored, 1, 1, 1)
    )
    header = pad(header, 100)
    return header + body + timedate(2026, 3, 15, 7, 0) + struct.pack("<I", xor32(body))


def bucket_descriptor_block(rrv_buckets, names, write_count):
    table, key_count, text_size = names.table()
    data = b"".join(struct.pack("<II", position >> 8 | non_data, first) for position, first, non_data in rrv_buckets) + table
    body = compressed_block(data)
    stored = BUCKET_HEADER_SIZE + len(body) + 12
    header = (
        struct.pack("<HHHIII", 0x0001, 2, 1, len(data), write_count, stored)
        + timedate(2026, 3, 15, 7, 0)
        + struct.pack("<IIIII", key_count, 1, text_size, len(rrv_buckets), 0)
        + b"\0" * 8
    )
    assert len(header) == 54
    checksum = xor32(header) ^ xor32(b"\0" * 8)
    header += struct.pack("<I", checksum) + b"\0" * 8
    return header + body + timedate(2026, 3, 15, 7, 0) + struct.pack("<I", xor32(body))


def database_header(title, superblock_at, superblock_size, bdb_at, bdb_size, file_size):
    info = pad(
        struct.pack("<I", ODS_VERSION)
        + struct.pack("<II", *CREATED)
        + b"\0" * 58
        + struct.pack("<HH", RRV_BUCKET_SIZE, SUMMARY_BUCKET_SIZE)
        + b"\0" * 8
        + struct.pack("<I", file_size >> 8),
        174,
    )
    replication = struct.pack("<II", *CREATED) + struct.pack("<HH", 0, 90) + timedate(2026, 1, 1, 0, 0)
    buffer = pad(lmbcs(title) + b"\n\0", 128)
    information2 = pad(
        timedate(2026, 3, 15, 7, 0)
        + b"\0" * 32
        + struct.pack("<II", superblock_at >> 8, superblock_size)
        + b"\0" * 24
        + b"\0" * 32
        + struct.pack("<IIII", bdb_size, bdb_at >> 8, 0, 0),
        124,
    )
    header = struct.pack("<HI", 0x001A, DATABASE_HEADER_SIZE) + info + replication + buffer + b"\0" * 128 + b"\0" * 64 + information2
    return pad(header, DATABASE_HEADER_SIZE)


def database(variant):
    names = Names()
    data_rrv = DATA_RRV_START
    non_data_rrv = NON_DATA_RRV_START
    object_rrv = non_data_rrv + 8
    summary_slots = []
    non_summary_slots = []

    def place(rrv, note_class, items):
        non_summary_id = 0x80000000 | (len(non_summary_slots) + 1) << 24 | 1
        summary, record = note(rrv, note_class, items, names, non_summary_id)
        summary_slots.append(summary)
        if record:
            non_summary_slots.append(record)
        return (1, len(summary_slots))

    data_entries = {
        data_rrv: place(data_rrv, 0x0001, entry(variant, object_rrv)),
        data_rrv + 4: place(data_rrv + 4, 0x0001, calm()),
    }
    summary_slots.append(stub(data_rrv + 8))
    data_entries[data_rrv + 8] = (1, len(summary_slots))
    non_data_entries = {non_data_rrv: place(non_data_rrv, 0x0004, form())}

    superblock_at = 0x400
    bdb_at = 0x8400
    rrv_at = 0x9400
    summary_at = rrv_at + 2 * RRV_BUCKET_SIZE
    non_summary_at = summary_at + SUMMARY_BUCKET_SIZE
    object_at = non_summary_at + NON_SUMMARY_BUCKET_SIZE
    non_data_entries[object_rrv] = object_at
    obj = struct.pack("<HIII", 0x0007, 22 + len(TIDE), 0x200, 1) + b"\0" * 8 + TIDE
    file_size = object_at + ((len(obj) + 255) // 256) * 256

    rrv_buckets = [(rrv_at, data_rrv, 0), (rrv_at + RRV_BUCKET_SIZE, non_data_rrv, 1)]
    block = superblock([summary_at], [non_summary_at], 7)
    bdb = bucket_descriptor_block(rrv_buckets, names, 9)
    image = bytearray(file_size)

    def put(at, data):
        image[at : at + len(data)] = data

    put(0, database_header("Lighthouse Log", superblock_at, 0x8000, bdb_at, 0x1000, file_size))
    put(superblock_at, block)
    put(bdb_at, bdb)
    put(rrv_at, rrv_bucket(data_rrv, data_entries))
    put(rrv_at + RRV_BUCKET_SIZE, rrv_bucket(non_data_rrv, non_data_entries))
    put(summary_at, bucket(summary_slots, SUMMARY_BUCKET_SIZE))
    put(non_summary_at, bucket(non_summary_slots, NON_SUMMARY_BUCKET_SIZE))
    put(object_at, obj)
    return bytes(image)


if __name__ == "__main__":
    variant, output = sys.argv[1], sys.argv[2]
    assert variant in ("log", "sealed", "lz1-attachment"), variant
    with open(output, "wb") as file:
        file.write(database(variant))
