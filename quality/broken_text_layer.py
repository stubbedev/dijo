import sys
import textwrap

FIRST_CODE = 0x20
LAST_CODE = 0xFF
LINE_WIDTH = 90
LINES_PER_PAGE = 52
BFCHAR_BLOCK = 100

MAPPINGS = {
    "private-use": lambda code: 0xE000 + code,
    "offset": lambda code: code if code == 0x20 else code - 29,
}


def lines_of(text):
    lines = []
    for paragraph in text.split("\n"):
        if paragraph.strip():
            lines.extend(textwrap.wrap(paragraph, LINE_WIDTH))
            lines.append("")
    return lines


def pdf_string(line):
    escaped = line.encode("cp1252").replace(b"\\", b"\\\\")
    return b"(" + escaped.replace(b"(", b"\\(").replace(b")", b"\\)") + b")"


def content_stream(lines):
    body = [b"BT /F1 11 Tf 14 TL 50 790 Td"]
    for line in lines:
        body.append(pdf_string(line) + b" Tj T*")
    body.append(b"ET")
    return b"\n".join(body)


def to_unicode(mapping):
    entries = [
        b"<%02X> <%04X>" % (code, mapping(code)) for code in range(FIRST_CODE, LAST_CODE + 1)
    ]
    blocks = []
    for start in range(0, len(entries), BFCHAR_BLOCK):
        block = entries[start:start + BFCHAR_BLOCK]
        blocks.append(b"%d beginbfchar\n" % len(block) + b"\n".join(block) + b"\nendbfchar")
    return b"\n".join([
        b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap",
        b"/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def",
        b"/CMapName /Adobe-Identity-UCS def /CMapType 2 def",
        b"1 begincodespacerange <00> <FF> endcodespacerange",
        *blocks,
        b"endcmap CMapName currentdict /CMap defineresource pop end end",
    ])


def stream(data):
    return b"<< /Length %d >>\nstream\n" % len(data) + data + b"\nendstream"


def document(text, mapping):
    lines = lines_of(text)
    pages = [lines[start:start + LINES_PER_PAGE] for start in range(0, len(lines), LINES_PER_PAGE)]
    first_page = 5
    kids = b" ".join(b"%d 0 R" % (first_page + 2 * index) for index in range(len(pages)))
    objects = [
        b"<< /Type /Catalog /Pages 2 0 R >>",
        b"<< /Type /Pages /Kids [%s] /Count %d >>" % (kids, len(pages)),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding"
        b" /ToUnicode 4 0 R >>",
        stream(to_unicode(mapping)),
    ]
    for index, page in enumerate(pages):
        objects.append(
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842]"
            b" /Resources << /Font << /F1 3 0 R >> >> /Contents %d 0 R >>"
            % (first_page + 2 * index + 1)
        )
        objects.append(stream(content_stream(page)))
    output = bytearray(b"%PDF-1.7\n")
    offsets = []
    for number, body in enumerate(objects, start=1):
        offsets.append(len(output))
        output += b"%d 0 obj\n" % number + body + b"\nendobj\n"
    xref = len(output)
    output += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objects) + 1)
    for offset in offsets:
        output += b"%010d 00000 n \n" % offset
    output += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (
        len(objects) + 1, xref)
    return bytes(output)


def main():
    if len(sys.argv) != 4 or sys.argv[3] not in MAPPINGS:
        sys.exit(f"usage: {sys.argv[0]} <truth.txt> <output.pdf> <{'|'.join(MAPPINGS)}>")
    truth, output, mapping = sys.argv[1:]
    with open(truth, encoding="utf-8") as source:
        text = source.read()
    with open(output, "wb") as target:
        target.write(document(text, MAPPINGS[mapping]))


main()
