"""Writes test_files/documents/lighthouse.dmg, an Apple disk image (UDIF) laid out as
`hdiutil create -layout GPTSPUD` lays one out: a protective MBR, a GUID partition table and its
backup, and one HFS Plus volume, "Lighthouse", of 64 blocks of 4 KiB, whose 512 sectors the
block map holds in chunks of 64 sectors each coded differently: zlib (UDZO), stored (raw),
bzip2 (UDBZ), ADC (UDCO), and one ignored (free space, read as zeros).

The volume, written here without hdiutil, holds:

    ReadMe.txt
    latest.txt -> ReadMe.txt                    a symbolic link
    shared.txt                                  a hard link to iNode30 of the private folder
    compressed.txt                              flagged UF_COMPRESSED, its data fork empty
    charts/soundings.bin                        37,864 bytes in ten one-block extents, the last
                                                two in the extents overflow file
    logbook/Café visits.txt               named decomposed, as HFS Plus stores names
    logbook/maintenance notes.txt
    logbook/shared again.txt                    a second hard link to iNode30
    logbook/1890s/Første storm.txt
    \0\0\0\0HFS+ Private Data/iNode30           the file both hard links stand for

Its catalog B-tree has nodes of 4 KiB in two extents, so that its records fill two leaf nodes
under an index node; its extents overflow B-tree, nodes of 4 KiB, one leaf. Keys are ordered as
HFS Plus orders them case-insensitively, a NUL after every other character. Dates are
2026-11-14T12:00:00Z; checksums are CRC-32s, the trailer's master checksum that of the
partitions' checksums.

Usage: lighthouse.py OUTPUT.dmg
"""

import base64
import bz2
import struct
import sys
import zlib

SECTOR = 512
BLOCK = 4096
BLOCKS = 64
NODE = 4096
CHUNK_SECTORS = 64
UNIX_TIME = 1_794_657_600
HFS_TIME = UNIX_TIME + 2_082_844_800

FOLDER_RECORD, FILE_RECORD, FOLDER_THREAD, FILE_THREAD = 1, 2, 3, 4
ROOT_PARENT, ROOT = 1, 2
EXTENTS_FILE, CATALOG_FILE = 3, 4

README = b"The Lighthouse Keeper\n\nEvery evening at dusk the keeper climbed the tower and lit the lamp.\n"
SHARED = b"This entry is shared by two names through a hard link.\n"
CAFE = "Visitors took coffee in the keeper's café on Sundays.\n".encode()
MAINTENANCE = b"Oil the lamp weekly and trim the wick each night.\n"
STORM = b"Storm from the north-west; the lamp burned until dawn.\n"
LINK_TARGET = b"ReadMe.txt"
SOUNDINGS_SIZE = 9 * BLOCK + 1000


def soundings():
    rows = bytearray()
    index = 0
    while len(rows) < SOUNDINGS_SIZE:
        rows += struct.pack(">IHHI", index, (index * 37) % 4000, 0xFFFF, 0)
        index += 1
    return bytes(rows[:SOUNDINGS_SIZE])


def fold(name):
    units = []
    for unit in struct.unpack(f">{len(name) // 2}H", name):
        if unit == 0:
            units.append(0xFFFF)
            continue
        lowered = chr(unit).lower()
        units.append(ord(lowered) if len(lowered) == 1 else unit)
    return units


def utf16(text):
    return text.encode("utf-16-be")


def catalog_key(parent, name):
    return struct.pack(">HIH", 6 + len(name), parent, len(name) // 2) + name


def permissions(mode, special=0, owner_flags=0):
    return struct.pack(">IIBBHI", 501, 20, 0, owner_flags, mode, special)


def dates():
    return struct.pack(">5I", HFS_TIME, HFS_TIME, HFS_TIME, HFS_TIME, 0)


def fork(size, extents, blocks=None):
    if blocks is None:
        blocks = sum(count for _, count in extents)
    padded = list(extents) + [(0, 0)] * (8 - len(extents))
    return struct.pack(">QII", size, 0, blocks) + b"".join(
        struct.pack(">II", start, count) for start, count in padded
    )


def folder_record(folder, valence):
    record = struct.pack(">hHII", FOLDER_RECORD, 0, valence, folder) + dates()
    record += permissions(0o040755) + bytes(32) + struct.pack(">II", 0, 0)
    assert len(record) == 88
    return record


def file_record(file_id, data_fork, mode=0o100644, special=0, owner_flags=0, kind=b"\0" * 4,
                creator=b"\0" * 4, flags=0x0002):
    record = struct.pack(">hHII", FILE_RECORD, flags, 0, file_id) + dates()
    record += permissions(mode, special, owner_flags)
    record += kind + creator + bytes(8) + bytes(16) + struct.pack(">II", 0, 0)
    record += data_fork + fork(0, [])
    assert len(record) == 248
    return record


def thread(kind, parent, name):
    return struct.pack(">hhIH", kind, 0, parent, len(name) // 2) + name


def node(kind, height, records, forward=0, backward=0):
    """A B-tree node of the records, its offsets table at its end."""
    body = struct.pack(">IIbBHH", forward, backward, kind, height, len(records), 0)
    offsets = []
    for record in records:
        offsets.append(len(body))
        body += record
    offsets.append(len(body))
    table = b"".join(struct.pack(">H", offset) for offset in reversed(offsets))
    assert len(body) + len(table) <= NODE, "records overflow the node"
    return body + bytes(NODE - len(body) - len(table)) + table


def header_node(depth, root, leaf_records, first_leaf, last_leaf, max_key, nodes, free,
                attributes, compare, map_bits):
    record = struct.pack(
        ">HIIIIHHIIHIBBI",
        depth, root, leaf_records, first_leaf, last_leaf, NODE, max_key, nodes, free, 0,
        NODE, 0, compare, attributes,
    ) + bytes(64)
    user = bytes(128)
    bitmap = bytearray(NODE - 14 - len(record) - len(user) - 8)
    for index in range(map_bits):
        bitmap[index // 8] |= 0x80 >> (index % 8)
    return node(1, 0, [record, user, bytes(bitmap)])


def tree(records, nodes, max_key, attributes, compare, split):
    """A B-tree of `nodes` nodes of the sorted (key, data) `records`, its leaves cut after each
    index of `split`, under one index node."""
    leaves = []
    start = 0
    for end in list(split) + [len(records)]:
        leaves.append(records[start:end])
        start = end
    count = len(leaves)
    first_leaf, index_node = 1, 1 + count
    built = [None] * nodes
    for number, leaf in enumerate(leaves, start=first_leaf):
        forward = number + 1 if number < count else 0
        backward = number - 1 if number > first_leaf else 0
        built[number] = node(-1, 1, [key + data for key, data in leaf], forward, backward)
    if count > 1:
        index_records = []
        for number, leaf in enumerate(leaves, start=first_leaf):
            key = leaf[0][0]
            if not attributes & 4:
                length = struct.unpack(">H", key[:2])[0]
                key = key + bytes(max_key - length)
            index_records.append(key + struct.pack(">I", number))
        built[index_node] = node(0, 2, index_records)
        root, depth, used = index_node, 2, count + 2
    else:
        root, depth, used = first_leaf, 1, 2
    built[0] = header_node(depth, root, len(records), first_leaf, count, max_key, nodes,
                           nodes - used, attributes, compare, used)
    return b"".join(page if page is not None else bytes(NODE) for page in built)


def gpt_header(current, backup, entries_at, entries, disk_sectors, first_usable, last_usable):
    header = bytearray(struct.pack(
        "<8sIIIIQQQQ16sQIII",
        b"EFI PART", 0x00010000, 92, 0, 0, current, backup, first_usable, last_usable,
        bytes(range(16)), entries_at, 128, 128, zlib.crc32(entries),
    ))
    header[16:20] = struct.pack("<I", zlib.crc32(bytes(header)))
    return bytes(header) + bytes(SECTOR - len(header))


def adc(data):
    """`data` coded with ADC: literals of up to 128 bytes, and matches of four bytes or more at
    the last place within 64 KiB its next three bytes stood before."""
    coded = bytearray()
    literal = bytearray()

    def flush():
        for at in range(0, len(literal), 128):
            run = literal[at:at + 128]
            coded.append(0x80 | (len(run) - 1))
            coded.extend(run)
        literal.clear()

    last = {}
    at = 0
    while at < len(data):
        key = data[at:at + 3]
        start = last.get(key) if len(key) == 3 else None
        if len(key) == 3:
            last[key] = at
        length = 0
        if start is not None and at - start <= 65536:
            while length < 67 and at + length < len(data) and data[start + length] == data[at + length]:
                length += 1
        if length >= 4:
            flush()
            offset = at - start - 1
            if length <= 18 and offset < 1024:
                coded.append(((length - 3) << 2) | (offset >> 8))
                coded.append(offset & 0xFF)
            else:
                coded.append(0x40 | (length - 4))
                coded.extend(struct.pack(">H", offset))
            at += length
        else:
            literal.append(data[at])
            at += 1
    flush()
    return bytes(coded)


def volume():
    image = bytearray(BLOCKS * BLOCK)

    def put(block, data):
        image[block * BLOCK:block * BLOCK + len(data)] = data

    sounding_blocks = [14, 16, 18, 20, 22, 24, 26, 28, 30, 32]
    data = soundings()
    for index, block in enumerate(sounding_blocks):
        put(block, data[index * BLOCK:(index + 1) * BLOCK])
    files = {
        20: (8, README),
        30: (9, SHARED),
        25: (10, CAFE),
        26: (11, MAINTENANCE),
        28: (12, STORM),
        21: (13, LINK_TARGET),
    }
    for block, content in files.values():
        put(block, content)

    def data_fork(file_id):
        block, content = files[file_id]
        return fork(len(content), [(block, 1)])

    names = {
        "root": utf16("Lighthouse"),
        "private": utf16("\0\0\0\0HFS+ Private Data"),
        "charts": utf16("charts"),
        "logbook": utf16("logbook"),
        "1890s": utf16("1890s"),
        "readme": utf16("ReadMe.txt"),
        "latest": utf16("latest.txt"),
        "shared": utf16("shared.txt"),
        "compressed": utf16("compressed.txt"),
        "soundings": utf16("soundings.bin"),
        "cafe": utf16("Café visits.txt"),
        "maintenance": utf16("maintenance notes.txt"),
        "again": utf16("shared again.txt"),
        "storm": utf16("Første storm.txt"),
        "inode": utf16("iNode30"),
    }
    folders = [
        (ROOT_PARENT, ROOT, "root", 6),
        (ROOT, 16, "private", 1),
        (ROOT, 17, "charts", 1),
        (ROOT, 18, "logbook", 4),
        (18, 19, "1890s", 1),
    ]
    link = dict(kind=b"hlnk", creator=b"hfs+", special=30, flags=0x0022)
    files_records = [
        (ROOT, 20, "readme", file_record(20, data_fork(20))),
        (ROOT, 21, "latest", file_record(21, data_fork(21), mode=0o120755, kind=b"slnk",
                                         creator=b"rhap")),
        (ROOT, 22, "shared", file_record(22, fork(0, []), **link)),
        (ROOT, 23, "compressed", file_record(23, fork(0, []), owner_flags=0x20)),
        (17, 24, "soundings", file_record(24, fork(
            SOUNDINGS_SIZE, [(block, 1) for block in sounding_blocks[:8]], blocks=10))),
        (18, 25, "cafe", file_record(25, data_fork(25))),
        (18, 26, "maintenance", file_record(26, data_fork(26))),
        (18, 27, "again", file_record(27, fork(0, []), **link)),
        (19, 28, "storm", file_record(28, data_fork(28))),
        (16, 30, "inode", file_record(30, data_fork(30), special=2, flags=0x0022)),
    ]
    records = []
    for parent, folder, name, valence in folders:
        records.append((catalog_key(parent, names[name]), folder_record(folder, valence)))
        records.append((catalog_key(folder, b""), thread(FOLDER_THREAD, parent, names[name])))
    for parent, file_id, name, record in files_records:
        records.append((catalog_key(parent, names[name]), record))
        records.append((catalog_key(file_id, b""), thread(FILE_THREAD, parent, names[name])))
    records.sort(key=lambda record: (
        struct.unpack(">I", record[0][2:6])[0], fold(record[0][8:])))
    catalog = tree(records, 8, 516, 6, 0xCF, [len(records) // 2])
    catalog_extents = [(4, 4), (40, 4)]
    put(4, catalog[:4 * NODE])
    put(40, catalog[4 * NODE:])

    overflow_key = struct.pack(">HBBII", 10, 0, 0, 24, 8)
    overflow_data = b"".join(struct.pack(">II", b, 1) for b in sounding_blocks[8:]) + bytes(48)
    extents = tree([(overflow_key, overflow_data)], 2, 10, 2, 0, [])
    put(2, extents)

    used = {0, 1, 2, 3, 4, 5, 6, 7, 40, 41, 42, 43, 63} | set(sounding_blocks)
    used |= {block for block, _ in files.values()}
    bitmap = bytearray(BLOCK)
    for block in used:
        bitmap[block // 8] |= 0x80 >> (block % 8)
    put(1, bitmap)

    header = struct.pack(">2sH" + "I" * 17 + "Q", b"H+", 4, 1 << 8, 0x31302E30, 0,
                         HFS_TIME, HFS_TIME, 0, HFS_TIME, 10, 4, BLOCK, BLOCKS,
                         BLOCKS - len(used), 44, 65536, 65536, 31, 1, 1)
    header += bytes(32)
    header += fork(BLOCK, [(1, 1)])
    header += fork(2 * NODE, [(2, 2)])
    header += fork(8 * NODE, catalog_extents)
    header += fork(0, []) + fork(0, [])
    assert len(header) == 512
    image[1024:1536] = header
    image[BLOCKS * BLOCK - 1024:BLOCKS * BLOCK - 512] = header
    return bytes(image)


def mbr(disk_sectors):
    sector = bytearray(SECTOR)
    sector[446:462] = struct.pack("<B3sB3sII", 0, b"\x00\x02\x00", 0xEE, b"\xff\xff\xff", 1,
                                  min(disk_sectors - 1, 0xFFFFFFFF))
    sector[510:512] = b"\x55\xaa"
    return bytes(sector)


def mish(first, sectors, runs, offset, number, checksum):
    """A mish table of the partition of `sectors` at `first`, of (kind, sector, sectors, data)
    `runs`, whose bytes start at `offset` of the data fork."""
    table = struct.pack(">4sIQQQII", b"mish", 1, first, sectors, 0, 0x208, number) + bytes(24)
    table += struct.pack(">II", 2, 32) + struct.pack(">I", checksum) + bytes(124)
    table += struct.pack(">I", len(runs) + 1)
    for kind, sector, count, data in runs:
        table += struct.pack(">IIQQQQ", kind, 0, sector, count, offset, len(data))
        offset += len(data)
    table += struct.pack(">IIQQQQ", 0xFFFFFFFF, 0, sectors, 0, offset, 0)
    return table


def main():
    hfs = volume()
    hfs_sectors = len(hfs) // SECTOR
    hfs_first = 40
    backup_table = hfs_first + hfs_sectors
    disk_sectors = backup_table + 32 + 1
    entry = bytearray(128)
    entry[0:16] = bytes.fromhex("005346480000aa11aa1100306543ecac")
    entry[16:32] = bytes(range(16, 32))
    entry[32:48] = struct.pack("<QQ", hfs_first, hfs_first + hfs_sectors - 1)
    entry[56:56 + 20] = "disk image".encode("utf-16-le")
    entries = bytes(entry) + bytes(127 * 128)
    primary = gpt_header(1, disk_sectors - 1, 2, entries, disk_sectors, 34, backup_table - 1)
    backup = gpt_header(disk_sectors - 1, 1, backup_table, entries, disk_sectors, 34,
                        backup_table - 1)

    def runs(data, codings):
        out = []
        for index, coding in enumerate(codings):
            piece = data[index * CHUNK_SECTORS * SECTOR:(index + 1) * CHUNK_SECTORS * SECTOR]
            sectors = len(piece) // SECTOR
            if coding == "zlib":
                out.append((0x80000005, index * CHUNK_SECTORS, sectors, zlib.compress(piece, 9)))
            elif coding == "raw":
                out.append((1, index * CHUNK_SECTORS, sectors, piece))
            elif coding == "bzip2":
                out.append((0x80000006, index * CHUNK_SECTORS, sectors, bz2.compress(piece, 9)))
            elif coding == "adc":
                out.append((0x80000004, index * CHUNK_SECTORS, sectors, adc(piece)))
            elif coding == "ignore":
                assert piece == bytes(len(piece)), "an ignored chunk holds data"
                out.append((2, index * CHUNK_SECTORS, sectors, b""))
        return out

    partitions = [
        ("Protective Master Boot Record (MBR : 0)", 0, mbr(disk_sectors), ["raw"]),
        ("GPT Header (Primary GPT Header : 1)", 1, primary, ["raw"]),
        ("GPT Partition Data (Primary GPT Table : 2)", 2, entries, ["zlib"]),
        (" (Apple_Free : 3)", 34, bytes(6 * SECTOR), ["ignore"]),
        ("disk image (Apple_HFS : 4)", hfs_first, hfs,
         ["zlib", "raw", "bzip2", "adc", "zlib", "adc", "ignore", "zlib"]),
        ("GPT Partition Data (Backup GPT Table : 5)", backup_table, entries, ["zlib"]),
        ("GPT Header (Backup GPT Header : 6)", disk_sectors - 1, backup, ["raw"]),
    ]
    fork_bytes = bytearray()
    plist = [
        '<?xml version="1.0" encoding="UTF-8"?>',
        '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" '
        '"http://www.apple.com/DTDs/PropertyList-1.0.dtd">',
        '<plist version="1.0">',
        "<dict>",
        "\t<key>resource-fork</key>",
        "\t<dict>",
        "\t\t<key>blkx</key>",
        "\t\t<array>",
    ]
    whole = bytearray()
    checksums = bytearray()
    for number, (name, first, data, codings) in enumerate(partitions):
        assert len(whole) == first * SECTOR, name
        whole += data
        chunk_runs = runs(data, codings)
        checksums += struct.pack(">I", zlib.crc32(data))
        table = mish(first, len(data) // SECTOR, chunk_runs, len(fork_bytes), number,
                     zlib.crc32(data))
        for _, _, _, coded in chunk_runs:
            fork_bytes += coded
        encoded = base64.b64encode(table).decode()
        lines = [encoded[at:at + 52] for at in range(0, len(encoded), 52)]
        plist += [
            "\t\t\t<dict>",
            "\t\t\t\t<key>Attributes</key>",
            "\t\t\t\t<string>0x0050</string>",
            "\t\t\t\t<key>CFName</key>",
            f"\t\t\t\t<string>{name}</string>",
            "\t\t\t\t<key>Data</key>",
            "\t\t\t\t<data>",
            *[f"\t\t\t\t{line}" for line in lines],
            "\t\t\t\t</data>",
            "\t\t\t\t<key>ID</key>",
            f"\t\t\t\t<string>{number - 1}</string>",
            "\t\t\t\t<key>Name</key>",
            f"\t\t\t\t<string>{name}</string>",
            "\t\t\t</dict>",
        ]
    assert len(whole) == disk_sectors * SECTOR
    plist += ["\t\t</array>", "\t</dict>", "</dict>", "</plist>", ""]
    xml = "\n".join(plist).encode()
    xml_at = len(fork_bytes)
    koly = struct.pack(">4sIIIQQQQQII16sII", b"koly", 4, 512, 1, 0, 0, len(fork_bytes), 0, 0, 1,
                       1, bytes(range(0x40, 0x50)), 2, 32)
    koly += struct.pack(">I", zlib.crc32(bytes(fork_bytes))) + bytes(124)
    koly += struct.pack(">QQ", xml_at, len(xml)) + bytes(120)
    koly += struct.pack(">II", 2, 32) + struct.pack(">I", zlib.crc32(bytes(checksums))) + bytes(124)
    koly += struct.pack(">IQ", 1, disk_sectors) + bytes(12)
    assert len(koly) == 512
    with open(sys.argv[1], "wb") as handle:
        handle.write(bytes(fork_bytes) + xml + koly)


main()
