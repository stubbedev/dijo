import sys
import textwrap
from pathlib import Path


def pairs(*items):
    return "".join(f"{code}\n{value}\n" for code, value in items)


def main():
    source, output = sys.argv[1:]
    paragraphs = [p for p in Path(source).read_text(encoding="utf-8").split("\n\n") if p.strip()]
    entities = []
    y = 0.0
    for paragraph in paragraphs:
        for line in textwrap.wrap(" ".join(paragraph.split()), 60):
            entities.append(
                pairs((0, "TEXT"), (8, "NOTES"), (10, "0.0"), (20, f"{y:.1f}"), (30, "0.0"), (40, "2.5"), (1, line))
            )
            y -= 4.0
        y -= 4.0
    with open(output, "w", encoding="ascii", newline="\r\n") as out:
        out.write(pairs((0, "SECTION"), (2, "HEADER"), (9, "$ACADVER"), (1, "AC1009"), (0, "ENDSEC")))
        out.write(pairs((0, "SECTION"), (2, "ENTITIES")))
        out.write("".join(entities))
        out.write(pairs((0, "ENDSEC"), (0, "EOF")))


main()
