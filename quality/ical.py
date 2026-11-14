import sys
from pathlib import Path


def fold(line):
    data = line.encode("utf-8")
    chunks = []
    while len(data) > 75:
        cut = 75 if not chunks else 74
        while data[cut] & 0xC0 == 0x80:
            cut -= 1
        chunks.append(data[:cut])
        data = data[cut:]
    chunks.append(data)
    return b"\r\n ".join(chunks).decode("utf-8")


def escape(text):
    return text.replace("\\", "\\\\").replace(";", "\\;").replace(",", "\\,")


def main():
    source, output = sys.argv[1:]
    events = [block.split("\n") for block in Path(source).read_text(encoding="utf-8").strip().split("\n\n")]
    lines = ["BEGIN:VCALENDAR", "VERSION:2.0", "PRODID:-//dijo quality corpus//EN", "CALSCALE:GREGORIAN"]
    for number, (summary, location, description) in enumerate(events, 1):
        lines += [
            "BEGIN:VEVENT",
            f"UID:event-{number}@example.org",
            "DTSTAMP:20240301T090000Z",
            f"DTSTART:202404{number * 7:02d}T100000Z",
            f"DTEND:202404{number * 7:02d}T120000Z",
            f"SUMMARY:{escape(summary)}",
            f"LOCATION:{escape(location)}",
            f"DESCRIPTION:{escape(description)}",
            "END:VEVENT",
        ]
    lines.append("END:VCALENDAR")
    with open(output, "w", encoding="utf-8", newline="") as out:
        out.write("".join(fold(line) + "\r\n" for line in lines))


main()
