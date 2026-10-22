import sys

LINE_WIDTH = 62
SENTENCE_ENDINGS = (".", "!", "?")


def gaps_of(words):
    return [2 if word.endswith(SENTENCE_ENDINGS) else 1 for word in words[:-1]]


def lines_of(words, width):
    lines = []
    current = []
    for word in words:
        candidate = current + [word]
        if current and len(" ".join(candidate)) > width:
            lines.append(current)
            current = [word]
        else:
            current = candidate
    if current:
        lines.append(current)
    return lines


def justified(words, width):
    gaps = gaps_of(words)
    if not gaps:
        return words[0]
    shortfall = width - sum(map(len, words)) - sum(gaps)
    first_widened = sum(map(len, words)) % len(gaps)
    for turn in range(max(shortfall, 0)):
        gaps[(first_widened + turn) % len(gaps)] += 1
    line = words[0]
    for gap, word in zip(gaps, words[1:]):
        line += " " * gap + word
    return line


def typewritten(paragraphs, width):
    lines = []
    for index, paragraph in enumerate(paragraphs):
        words = paragraph.split()
        if index == 0:
            lines.extend([paragraph.center(width).rstrip(), "", ""])
            continue
        words[0] = "     " + words[0]
        wrapped = lines_of(words, width)
        for line in wrapped[:-1]:
            lines.append(justified(line, width))
        last = wrapped[-1]
        lines.append(
            "".join(word + " " * gap for word, gap in zip(last, gaps_of(last) + [0]))
        )
        lines.append("")
    return "\n".join(lines).rstrip("\n") + "\n"


def main():
    source, output = sys.argv[1:]
    with open(source, encoding="utf-8") as text:
        paragraphs = [line.strip() for line in text if line.strip()]
    with open(output, "w", encoding="utf-8") as page:
        page.write(typewritten(paragraphs, LINE_WIDTH))


if __name__ == "__main__":
    main()
