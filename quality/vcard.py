import sys
from pathlib import Path


def escape(text):
    return text.replace("\\", "\\\\").replace(";", "\\;").replace(",", "\\,")


def main():
    source, output = sys.argv[1:]
    cards = [block.split("\n") for block in Path(source).read_text(encoding="utf-8").strip().split("\n\n")]
    lines = []
    for name, organisation, title, email, telephone, address, note in cards:
        given, family = name.split(" ", 1)
        street, locality = address.rsplit(", ", 1)
        lines += [
            "BEGIN:VCARD",
            "VERSION:3.0",
            f"N:{escape(family)};{escape(given)};;;",
            f"FN:{escape(name)}",
            f"ORG:{escape(organisation)}",
            f"TITLE:{escape(title)}",
            f"EMAIL;TYPE=INTERNET:{email}",
            f"TEL;TYPE=WORK,VOICE:{telephone}",
            f"ADR;TYPE=WORK:;;{escape(street)};{escape(locality)};;;",
            f"NOTE:{escape(note)}",
            "END:VCARD",
        ]
    with open(output, "w", encoding="utf-8", newline="") as out:
        out.write("".join(line + "\r\n" for line in lines))


main()
