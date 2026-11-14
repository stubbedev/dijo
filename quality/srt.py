import sys
from pathlib import Path


def stamp(seconds):
    return f"00:{seconds // 60:02d}:{seconds % 60:02d},000"


def main():
    source, output = sys.argv[1:]
    paragraphs = [" ".join(p.split()) for p in Path(source).read_text(encoding="utf-8").split("\n\n") if p.strip()]
    with open(output, "w", encoding="utf-8") as out:
        for number, paragraph in enumerate(paragraphs, 1):
            start = (number - 1) * 8
            out.write(f"{number}\n{stamp(start)} --> {stamp(start + 7)}\n{paragraph}\n\n")


main()
