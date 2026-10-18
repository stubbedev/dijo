"""Encrypts the plain Word and Excel documents LibreOffice writes as Office does, with XOR
obfuscation and with RC4 CryptoAPI ([MS-OFFCRYPTO] 2.3.5 to 2.3.7, [MS-DOC] 2.2.6, [MS-XLS]
2.2.10), and checks every encryption msoffcrypto-tool decrypts against what it decrypts.

It also writes the encryptions [MS-OFFCRYPTO] 2.3.1 does not allow, which no Office application
writes and nothing decrypts: AES in a Word document and a workbook, their data left plain, and RC4
CryptoAPI in an OOXML package.

Usage: encrypt.py PLAIN.doc PLAIN.xls OUTPUT_DIRECTORY
"""

import hashlib
import io
import struct
import sys
import zipfile
from pathlib import Path

import msoffcrypto
import olefile
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

PASSWORD = "tika"

BOF = 0x0809
FILEPASS = 0x002F
BOUNDSHEET = 0x0085
INDEX = 0x020B
EXTSST = 0x00FF
INTERFACEHDR = 0x00E1
USREXCL = 0x0194
FILELOCK = 0x0195
RRDINFO = 0x0196
RRDHEAD = 0x0138
PLAIN_RECORDS = {BOF, FILEPASS, INTERFACEHDR, USREXCL, FILELOCK, RRDINFO, RRDHEAD}

FIB_BASE = 68
F_ENCRYPTED = 0x0100
F_WHICH_TBL_STM = 0x0200
F_OBFUSCATED = 0x8000
FC_LCB_COUNT = 152
FC_LCB_START = 154

INITIAL_CODE = [
    0xE1F0, 0x1D0F, 0xCC9C, 0x84C0, 0x110C, 0x0E10, 0xF1CE, 0x313E,
    0x1872, 0xE139, 0xD40F, 0x84F9, 0x280C, 0xA96A, 0x4EC3,
]
XOR_MATRIX = [
    0xAEFC, 0x4DD9, 0x9BB2, 0x2745, 0x4E8A, 0x9D14, 0x2A09, 0x7B61, 0xF6C2, 0xFDA5,
    0xEB6B, 0xC6F7, 0x9DCF, 0x2BBF, 0x4563, 0x8AC6, 0x05AD, 0x0B5A, 0x16B4, 0x2D68,
    0x5AD0, 0x0375, 0x06EA, 0x0DD4, 0x1BA8, 0x3750, 0x6EA0, 0xDD40, 0xD849, 0xA0B3,
    0x5147, 0xA28E, 0x553D, 0xAA7A, 0x44D5, 0x6F45, 0xDE8A, 0xAD35, 0x4A4B, 0x9496,
    0x390D, 0x721A, 0xEB23, 0xC667, 0x9CEF, 0x29FF, 0x53FE, 0xA7FC, 0x5FD9, 0x47D3,
    0x8FA6, 0x0F6D, 0x1EDA, 0x3DB4, 0x7B68, 0xF6D0, 0xB861, 0x60E3, 0xC1C6, 0x93AD,
    0x377B, 0x6EF6, 0xDDEC, 0x45A0, 0x8B40, 0x06A1, 0x0D42, 0x1A84, 0x3508, 0x6A10,
    0xAA51, 0x4483, 0x8906, 0x022D, 0x045A, 0x08B4, 0x1168, 0x76B4, 0xED68, 0xCAF1,
    0x85C3, 0x1BA7, 0x374E, 0x6E9C, 0x3730, 0x6E60, 0xDCC0, 0xA9A1, 0x4363, 0x86C6,
    0x1DAD, 0x3331, 0x6662, 0xCCC4, 0x89A9, 0x0373, 0x06E6, 0x0DCC, 0x1021, 0x2042,
    0x4084, 0x8108, 0x1231, 0x2462, 0x48C4,
]
PAD_ARRAY = [0xBB, 0xFF, 0xFF, 0xBA, 0xFF, 0xFF, 0xB9, 0x80, 0x00, 0xBE, 0x0F, 0x00, 0xBF, 0x0F, 0x00]


def ror(byte, count):
    return ((byte >> count) | (byte << (8 - count))) & 0xFF


def rol(byte, count):
    return ror(byte, 8 - count)


def ansi_password(password):
    """[MS-OFFCRYPTO] 2.3.7.4's second approach: each character's low byte, else its high byte,
    the first 15 of them."""
    return bytes((code & 0xFF) or (code >> 8) for code in map(ord, password[:15]))


def xor_key(password):
    """CreateXorKey_Method1, [MS-OFFCRYPTO] 2.3.7.2."""
    key = INITIAL_CODE[len(password) - 1]
    element = 0x68
    for byte in reversed(password):
        for _ in range(7):
            if byte & 0x40:
                key ^= XOR_MATRIX[element]
            byte = (byte << 1) & 0xFF
            element -= 1
    return key


def xor_verifier(password):
    """CreatePasswordVerifier_Method1, [MS-OFFCRYPTO] 2.3.7.1."""
    verifier = 0
    for byte in reversed(bytes([len(password)]) + password):
        verifier = (((verifier >> 14) & 1) | ((verifier << 1) & 0x7FFF)) ^ byte
    return verifier ^ 0xCE4B


def xor_array(password):
    """CreateXorArray_Method1, [MS-OFFCRYPTO] 2.3.7.2."""
    key = xor_key(password)
    high, low = key >> 8, key & 0xFF
    array = [0] * 16
    index = len(password)
    if index % 2 == 1:
        array[index] = ror(PAD_ARRAY[0] ^ high, 1)
        index -= 1
        array[index] = ror(password[-1] ^ low, 1)
    while index > 0:
        index -= 1
        array[index] = ror(password[index] ^ high, 1)
        index -= 1
        array[index] = ror(password[index] ^ low, 1)
    index = 15
    pad = 15 - len(password)
    while pad > 0:
        array[index] = ror(PAD_ARRAY[pad] ^ high, 1)
        index -= 1
        pad -= 1
        array[index] = ror(PAD_ARRAY[pad] ^ low, 1)
        index -= 1
        pad -= 1
    return array


def rc4(key, data):
    state = list(range(256))
    j = 0
    for i in range(256):
        j = (j + state[i] + key[i % len(key)]) & 0xFF
        state[i], state[j] = state[j], state[i]
    out = bytearray(data)
    i = j = 0
    for at in range(len(out)):
        i = (i + 1) & 0xFF
        j = (j + state[i]) & 0xFF
        state[i], state[j] = state[j], state[i]
        out[at] ^= state[(state[i] + state[j]) & 0xFF]
    return out


class CryptoApi:
    """RC4 CryptoAPI encryption, [MS-OFFCRYPTO] 2.3.5, with SHA-1 and a key of `key_bits`."""

    CSP = "Microsoft Enhanced Cryptographic Provider v1.0\0"
    FLAGS = 0x04 | 0x08

    def __init__(self, password, key_bits, version, salt_seed):
        self.key_bits = key_bits
        self.version = version
        self.salt = hashlib.sha1(salt_seed.encode()).digest()[:16]
        self.secret = hashlib.sha1(self.salt + password.encode("utf-16-le")).digest()

    def block_key(self, block):
        key = hashlib.sha1(self.secret + struct.pack("<I", block)).digest()[: self.key_bits // 8]
        return key + bytes(16 - len(key)) if self.key_bits == 40 else key

    def info(self):
        header = struct.pack(
            "<iiiiiiii", self.FLAGS, 0, 0x6801, 0x8004, self.key_bits, 1, 0, 0
        ) + self.CSP.encode("utf-16-le")
        verifier = hashlib.sha1(self.salt[::-1]).digest()[:16]
        encrypted = rc4(self.block_key(0), verifier + hashlib.sha1(verifier).digest())
        return (
            struct.pack("<HHiI", self.version, 2, self.FLAGS, len(header))
            + header
            + struct.pack("<I", 16)
            + self.salt
            + encrypted[:16]
            + struct.pack("<I", 20)
            + encrypted[16:]
        )

    def encrypt(self, data, start, chunk, plain=lambda position: False):
        """`data` from `start` encrypted by position, each chunk of `chunk` bytes with its block's
        key, the bytes `plain` names left as they are."""
        out = bytearray(data)
        for block in range(start // chunk, (len(data) + chunk - 1) // chunk):
            stream = rc4(self.block_key(block), bytes(chunk))
            for position in range(max(start, block * chunk), min(len(data), (block + 1) * chunk)):
                if not plain(position):
                    out[position] ^= stream[position - block * chunk]
        return bytes(out)


def records(workbook):
    at = 0
    while at + 4 <= len(workbook):
        sid, length = struct.unpack_from("<HH", workbook, at)
        yield sid, at, length
        at += 4 + length


def with_file_pass(workbook, file_pass):
    """The workbook with a FILEPASS record after its first BOF, the stream offsets in its
    BOUNDSHEET, INDEX and EXTSST records moved past it."""
    _, _, bof_length = next(records(workbook))
    record = struct.pack("<HH", FILEPASS, len(file_pass)) + file_pass
    split = 4 + bof_length
    moved = bytearray(workbook[:split] + record + workbook[split:])
    shift = len(record)

    def move(at):
        struct.pack_into("<I", moved, at, struct.unpack_from("<I", moved, at)[0] + shift)

    for sid, at, length in records(bytes(moved)):
        body = at + 4
        if sid == BOUNDSHEET:
            move(body)
        elif sid == INDEX:
            move(body + 12)
            for offset in range(body + 16, body + length, 4):
                move(offset)
        elif sid == EXTSST:
            for offset in range(body + 2, body + length, 8):
                move(offset)
    return bytes(moved)


def plain_in_workbook(workbook):
    """The positions of a workbook's bytes Excel leaves plain: every record header, the data of
    the records [MS-XLS] 2.2.10 names and the lbPlyPos of each BOUNDSHEET."""
    plain = set()
    for sid, at, length in records(workbook):
        plain.update(range(at, at + 4))
        if sid in PLAIN_RECORDS:
            plain.update(range(at + 4, at + 4 + length))
        elif sid == BOUNDSHEET:
            plain.update(range(at + 4, at + 8))
    return plain


def xls_crypto_api(workbook, api):
    """The workbook with its FILEPASS record, and encrypted with RC4 CryptoAPI."""
    plain = with_file_pass(workbook, struct.pack("<H", 1) + api.info())
    kept = plain_in_workbook(plain)
    return plain, api.encrypt(plain, file_pass_end(plain), 1024, kept.__contains__)


def xls_xor(workbook, password):
    """The workbook with its FILEPASS record, and obfuscated with XOR data transformation
    method 1, [MS-OFFCRYPTO] 2.3.7.3."""
    ansi = ansi_password(password)
    plain = with_file_pass(workbook, struct.pack("<HHH", 0, xor_key(ansi), xor_verifier(ansi)))
    array = xor_array(ansi)
    out = bytearray(plain)
    start = file_pass_end(plain)
    kept = plain_in_workbook(plain)
    for _, at, length in records(plain):
        for position in range(max(at + 4, start), at + 4 + length):
            if position not in kept:
                out[position] = rol(out[position], 5) ^ array[(position + length) % 16]
    return plain, bytes(out)


def file_pass_end(workbook):
    for sid, at, length in records(workbook):
        if sid == FILEPASS:
            return at + 4 + length
    raise ValueError("the workbook has no FILEPASS record")


def table_name(main):
    flags = struct.unpack_from("<H", main, 0xA)[0]
    return "1Table" if flags & F_WHICH_TBL_STM else "0Table"


def with_flags(main, flags, l_key):
    out = bytearray(main)
    old = struct.unpack_from("<H", out, 0xA)[0]
    struct.pack_into("<H", out, 0xA, old | flags)
    struct.pack_into("<I", out, 0xE, l_key)
    return out


def with_table_header(streams, header):
    """The document with `header` at the start of its table stream, `lKey` its length and the
    table's offsets in the FIB moved past it."""
    main = streams["WordDocument"]
    table = table_name(main)
    main = with_flags(main, F_ENCRYPTED, len(header))
    count = struct.unpack_from("<H", main, FC_LCB_COUNT)[0]
    for pair in range(count):
        at = FC_LCB_START + 8 * pair
        fc, lcb = struct.unpack_from("<II", main, at)
        if fc or lcb:
            struct.pack_into("<I", main, at, fc + len(header))
    return {**streams, "WordDocument": bytes(main), table: header + streams[table]}


def doc_crypto_api(streams, api):
    header = api.info()
    plain = with_table_header(streams, header)
    starts = {"WordDocument": FIB_BASE, table_name(plain["WordDocument"]): len(header), "Data": 0}
    encrypted = {
        name: api.encrypt(data, starts[name], 512) if name in starts else data
        for name, data in plain.items()
    }
    return plain, encrypted


def aes_info(password):
    """Standard encryption with AES-128 and SHA-1, [MS-OFFCRYPTO] 2.3.4.5 to 2.3.4.9."""
    flags = 0x04 | 0x20
    salt = hashlib.sha1(b"aes").digest()[:16]
    hashed = hashlib.sha1(salt + password.encode("utf-16-le")).digest()
    for iteration in range(50000):
        hashed = hashlib.sha1(struct.pack("<I", iteration) + hashed).digest()
    final = hashlib.sha1(hashed + struct.pack("<I", 0)).digest()
    key = hashlib.sha1(bytes(0x36 ^ byte for byte in final.ljust(64, b"\0"))).digest()[:16]
    encryptor = Cipher(algorithms.AES(key), modes.ECB()).encryptor()
    verifier = hashlib.sha1(salt[::-1]).digest()[:16]
    encrypted = encryptor.update(verifier + hashlib.sha1(verifier).digest().ljust(32, b"\0"))
    header = struct.pack("<iiiiiiii", flags, 0, 0x660E, 0x8004, 128, 0x18, 0, 0)
    header += "Microsoft Enhanced RSA and AES Cryptographic Provider\0".encode("utf-16-le")
    return (
        struct.pack("<HHiI", 4, 2, flags, len(header))
        + header
        + struct.pack("<I", 16)
        + salt
        + encrypted[:16]
        + struct.pack("<I", 20)
        + encrypted[16:]
    )


def ooxml_crypto_api(api):
    """A package of no parts but its content types, encrypted with RC4 CryptoAPI in blocks of 512
    bytes as a binary document's streams are."""
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as package:
        package.writestr(zipfile.ZipInfo("[Content_Types].xml"), "<Types/>")
    package = buffer.getvalue()
    return {
        "EncryptionInfo": api.info(),
        "EncryptedPackage": struct.pack("<Q", len(package)) + api.encrypt(package, 0, 512),
    }


def word_xor(data, start, array):
    """XOR data transformation method 2, [MS-OFFCRYPTO] 2.3.7.6, from `start`."""
    out = bytearray(data)
    for position in range(start, len(out)):
        obfuscated = out[position] ^ array[position % 16]
        if out[position] and obfuscated:
            out[position] = obfuscated
    return bytes(out)


def doc_xor(streams, password):
    ansi = ansi_password(password)
    array = xor_array(ansi)
    main = streams["WordDocument"]
    table = table_name(main)
    main = with_flags(main, F_ENCRYPTED | F_OBFUSCATED, (xor_key(ansi) << 16) | xor_verifier(ansi))
    out = dict(streams)
    out["WordDocument"] = word_xor(bytes(main), FIB_BASE, array)
    out[table] = word_xor(streams[table], 0, array)
    if "Data" in streams:
        out["Data"] = word_xor(streams["Data"], 0, array)
    return out


def read_streams(path):
    with olefile.OleFileIO(path) as ole:
        return {"/".join(entry): ole.openstream(entry).read() for entry in ole.listdir()}


class Compound:
    """A compound file of version 3 ([MS-CFB]) holding `streams` by their paths."""

    SECTOR = 512
    MINI = 64
    CUTOFF = 4096
    FREE, END, FAT = 0xFFFFFFFF, 0xFFFFFFFE, 0xFFFFFFFD

    def __init__(self, streams):
        self.streams = streams

    def tree(self):
        root = {"name": "Root Entry", "type": 5, "children": {}}
        for path, data in self.streams.items():
            parent = root
            *storages, name = path.split("/")
            for storage in storages:
                parent = parent["children"].setdefault(
                    storage, {"name": storage, "type": 1, "children": {}}
                )
            parent["children"][name] = {"name": name, "type": 2, "data": data}
        return root

    @staticmethod
    def order(entry):
        return (len(entry["name"]), entry["name"].upper())

    def write(self):
        entries = []

        def number(entry):
            entry["id"] = len(entries)
            entries.append(entry)
            children = sorted(entry.get("children", {}).values(), key=self.order)
            for child in children:
                number(child)
            entry["child"] = balanced(children)

        def balanced(children):
            if not children:
                return self.FREE
            middle = len(children) // 2
            node = children[middle]
            node["left"] = balanced(children[:middle])
            node["right"] = balanced(children[middle + 1 :])
            return node["id"]

        root = self.tree()
        number(root)
        mini_stream = bytearray()
        mini_fat = []
        large = []
        for entry in entries:
            data = entry.get("data")
            if data is None:
                continue
            if len(data) < self.CUTOFF:
                if not data:
                    entry["start"] = self.END
                    continue
                entry["start"] = len(mini_stream) // self.MINI
                count = -(-len(data) // self.MINI)
                mini_fat.extend(range(entry["start"] + 1, entry["start"] + count))
                mini_fat.append(self.END)
                mini_stream += data + bytes(count * self.MINI - len(data))
            else:
                large.append((entry, data))
        root["data_size"] = len(mini_stream)
        if mini_stream:
            large.append((root, bytes(mini_stream)))
        sectors = []
        fat = []

        def place(data):
            start = len(sectors)
            count = -(-len(data) // self.SECTOR)
            for k in range(count):
                sectors.append(data[k * self.SECTOR : (k + 1) * self.SECTOR].ljust(self.SECTOR, b"\0"))
                fat.append(start + k + 1 if k + 1 < count else self.END)
            return start

        for entry, data in large:
            entry["start"] = place(data)
        if not mini_stream:
            root["start"] = self.END
        mini_fat_start = self.END
        mini_fat_sectors = -(-len(mini_fat) // 128)
        if mini_fat:
            words = mini_fat + [self.FREE] * (mini_fat_sectors * 128 - len(mini_fat))
            mini_fat_start = place(struct.pack(f"<{len(words)}I", *words))
        directory = b"".join(self.directory_entry(entry) for entry in entries)
        directory += b"".join(self.directory_entry(None) for _ in range(-len(entries) % 4))
        directory_start = place(directory)
        fat_sectors = 0
        while fat_sectors * 128 < len(sectors) + fat_sectors:
            fat_sectors += 1
        if fat_sectors > 109:
            raise ValueError("too many FAT sectors for a header without DIFAT sectors")
        fat_start = len(sectors)
        fat.extend([self.FAT] * fat_sectors)
        fat.extend([self.FREE] * (fat_sectors * 128 - len(fat)))
        fat_bytes = struct.pack(f"<{len(fat)}I", *fat)
        for k in range(fat_sectors):
            sectors.append(fat_bytes[k * self.SECTOR : (k + 1) * self.SECTOR])
        difat = list(range(fat_start, fat_start + fat_sectors)) + [self.FREE] * (109 - fat_sectors)
        header = (
            bytes.fromhex("D0CF11E0A1B11AE1")
            + bytes(16)
            + struct.pack("<HHHHH", 0x3E, 3, 0xFFFE, 9, 6)
            + bytes(6)
            + struct.pack(
                "<IIIIIIIII",
                0,
                fat_sectors,
                directory_start,
                0,
                self.CUTOFF,
                mini_fat_start,
                mini_fat_sectors,
                self.END,
                0,
            )
            + struct.pack("<109I", *difat)
        )
        return header + b"".join(sectors)

    def directory_entry(self, entry):
        if entry is None:
            return bytes(64) + struct.pack("<HBBIII", 0, 0, 1, self.FREE, self.FREE, self.FREE) + bytes(52)
        name = (entry["name"] + "\0").encode("utf-16-le")
        size = entry.get("data_size", len(entry.get("data", b"")))
        start = entry.get("start", 0)
        return (
            name.ljust(64, b"\0")
            + struct.pack(
                "<HBBIII", len(name), entry["type"], 1, entry.get("left", self.FREE),
                entry.get("right", self.FREE), entry["child"],
            )
            + bytes(16 + 4 + 16)
            + struct.pack("<IQ", start, size)
        )


def decrypted_by_msoffcrypto(data):
    office = msoffcrypto.OfficeFile(io.BytesIO(data))
    office.load_key(password=PASSWORD)
    out = io.BytesIO()
    office.decrypt(out)
    return read_streams(io.BytesIO(out.getvalue()))


def check_workbook(name, encrypted, expected):
    decrypted = decrypted_by_msoffcrypto(encrypted)["Workbook"]
    if len(decrypted) != len(expected):
        raise AssertionError(f"{name}: msoffcrypto-tool decrypts a workbook of another length")
    plain_spans = plain_in_workbook(expected)
    for sid, at, length in records(expected):
        span = range(at, at + 4 + length)
        if sid == FILEPASS:
            continue
        differing = [p for p in span if p not in plain_spans and decrypted[p] != expected[p]]
        if differing:
            raise AssertionError(f"{name}: msoffcrypto-tool decrypts record {sid:#x} otherwise")


def check_document(name, encrypted, expected, header_length):
    decrypted = decrypted_by_msoffcrypto(encrypted)
    table = table_name(expected["WordDocument"])
    if decrypted["WordDocument"][FIB_BASE:] != expected["WordDocument"][FIB_BASE:]:
        raise AssertionError(f"{name}: msoffcrypto-tool decrypts the main stream otherwise")
    if decrypted[table][header_length:] != expected[table][header_length:]:
        raise AssertionError(f"{name}: msoffcrypto-tool decrypts the table stream otherwise")
    if decrypted.get("Data") != expected.get("Data"):
        raise AssertionError(f"{name}: msoffcrypto-tool decrypts the data stream otherwise")


def main(doc_path, xls_path, out_directory):
    out = Path(out_directory)
    document = read_streams(doc_path)
    workbook = read_streams(xls_path)

    plain, encrypted = xls_crypto_api(workbook["Workbook"], CryptoApi(PASSWORD, 40, 2, "xls"))
    file = Compound({**workbook, "Workbook": encrypted}).write()
    check_workbook("cryptoapi.xls", file, plain)
    (out / "cryptoapi.xls").write_bytes(file)

    plain, encrypted = xls_xor(workbook["Workbook"], PASSWORD)
    file = Compound({**workbook, "Workbook": encrypted}).write()
    check_workbook("xor.xls", file, plain)
    (out / "xor.xls").write_bytes(file)

    api = CryptoApi(PASSWORD, 128, 4, "doc")
    plain, encrypted = doc_crypto_api(document, api)
    file = Compound(encrypted).write()
    check_document("cryptoapi.doc", file, plain, len(api.info()))
    (out / "cryptoapi.doc").write_bytes(file)

    (out / "xor.doc").write_bytes(Compound(doc_xor(document, PASSWORD)).write())

    aes = aes_info(PASSWORD)
    (out / "aes.doc").write_bytes(Compound(with_table_header(document, aes)).write())
    plain = with_file_pass(workbook["Workbook"], struct.pack("<H", 1) + aes)
    (out / "aes.xls").write_bytes(Compound({**workbook, "Workbook": plain}).write())
    api = CryptoApi(PASSWORD, 128, 4, "docx")
    (out / "rc4.docx").write_bytes(Compound(ooxml_crypto_api(api)).write())


if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    main(*sys.argv[1:])
