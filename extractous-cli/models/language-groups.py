import random
import re
import sys
import unicodedata
from pathlib import Path

import pyarrow.parquet as pq

WIKIPEDIA_REVISION = "b04c8d1ceb2f5cd4588862100d08de323dccfbaa"
SENTENCES = 100_000
MIN_SENTENCE = 30
MAX_SENTENCE = 300
MIN_PARAGRAPH = 80
MAX_DIGIT_SHARE = 0.1
MAX_TABLE_BARS = 2
SEED = 20261014
SENTENCE_END = re.compile(r"[.!?]\s+")

CYRILLIC_TO_LATIN = {
    "а": "a", "б": "b", "в": "v", "г": "g", "д": "d", "ђ": "đ", "е": "e", "ж": "ž", "з": "z", "и": "i",
    "ј": "j", "к": "k", "л": "l", "љ": "lj", "м": "m", "н": "n", "њ": "nj", "о": "o", "п": "p", "р": "r",
    "с": "s", "т": "t", "ћ": "ć", "у": "u", "ф": "f", "х": "h", "ц": "c", "ч": "č", "џ": "dž", "ш": "š",
}
CYRILLIC_TO_LATIN.update({k.upper(): v.capitalize() for k, v in list(CYRILLIC_TO_LATIN.items())})


def is_cyrillic(text):
    letters = [c for c in text if c.isalpha()]
    return sum("CYRILLIC" in unicodedata.name(c, "") for c in letters) * 2 > len(letters)


def sentences_of(paragraph):
    start = 0
    for end in SENTENCE_END.finditer(paragraph):
        if end.end() < len(paragraph) and paragraph[end.end()].isupper():
            yield paragraph[start:end.start() + 1].strip()
            start = end.end()
    yield paragraph[start:].strip()


def usable(sentence):
    digits = sum(c.isdigit() for c in sentence)
    return MIN_SENTENCE <= len(sentence) <= MAX_SENTENCE and digits <= len(sentence) * MAX_DIGIT_SHARE


def taken(sentence, selection):
    if selection == "as-is":
        return sentence
    if selection == "cyrillic-only":
        return sentence if is_cyrillic(sentence) else None
    if selection == "latin-from-cyrillic":
        return "".join(CYRILLIC_TO_LATIN.get(c, c) for c in sentence) if is_cyrillic(sentence) else None
    raise ValueError(f"unknown text selection {selection}")


def sample(shard, selection, seed):
    articles = pq.read_table(shard, columns=["text"]).column("text").to_pylist()
    rng = random.Random(seed)
    rng.shuffle(articles)
    found = []
    for article in articles:
        for paragraph in article.split("\n"):
            if len(paragraph) < MIN_PARAGRAPH or paragraph.count("|") > MAX_TABLE_BARS:
                continue
            for sentence in sentences_of(paragraph):
                text = taken(sentence, selection) if usable(sentence) else None
                if text:
                    found.append(" ".join(text.split()))
        if len(found) >= SENTENCES * 3:
            break
    rng.shuffle(found)
    return found[:SENTENCES]


def main(sources, shards, output):
    for line in Path(sources).read_text().splitlines():
        if not line or line.startswith("#"):
            continue
        group, _, label, shard, _, selection, _ = line.split("\t")
        target = Path(output) / group / f"{label}.txt"
        target.parent.mkdir(parents=True, exist_ok=True)
        sentences = sample(Path(shards) / shard, selection, f"{SEED}-{group}-{label}")
        partial = target.with_suffix(".partial")
        partial.write_text("".join(sentence + "\n" for sentence in sentences), encoding="utf-8")
        partial.replace(target)
        print(f"{group} {label}: {len(sentences)} sentences", file=sys.stderr)


if __name__ == "__main__":
    if len(sys.argv) != 4:
        sys.exit("usage: language-groups.py <language-groups.tsv> <shard directory> <sentence directory>")
    main(*sys.argv[1:])
