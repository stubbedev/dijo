import sys
from importlib.metadata import version
from pathlib import Path

NAMES = {"kreuzberg": "Kreuzberg", "docling": "Docling"}


def corpus_files(corpus_tsv):
    rows = Path(corpus_tsv).read_text(encoding="utf-8").splitlines()[1:]
    files = []
    for row in rows:
        file, _truth, _category, _helper, arguments = row.split("\t")
        if not arguments and file not in files:
            files.append(file)
    return files


def extractor(tool):
    if tool == "kreuzberg":
        from kreuzberg import extract_file_sync

        return lambda path: extract_file_sync(str(path)).content
    if tool == "docling":
        from docling.document_converter import DocumentConverter

        converter = DocumentConverter()
        return lambda path: converter.convert(str(path)).document.export_to_text()
    sys.exit(f"unknown tool {tool}")


def main():
    if len(sys.argv) != 5:
        sys.exit(f"usage: {sys.argv[0]} <kreuzberg|docling> <corpus dir> <corpus.tsv> <tools dir>")
    tool, corpus, corpus_tsv, tools = sys.argv[1:]
    extract = extractor(tool)
    out = Path(tools) / f"{NAMES[tool]} {version(tool)}"
    out.mkdir(parents=True, exist_ok=True)
    for file in corpus_files(corpus_tsv):
        try:
            text = extract(Path(corpus) / file)
        except Exception as error:
            print(f"{tool}: {file}: {error!r}", file=sys.stderr)
            continue
        (out / f"{file}.txt").write_text(text, encoding="utf-8")


main()
