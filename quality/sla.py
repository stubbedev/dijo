import sys
from pathlib import Path

import scribus

source, output = sys.argv[1:]
paragraphs = [" ".join(p.split()) for p in Path(source).read_text(encoding="utf-8").split("\n\n") if p.strip()]
margin = 57
width, height = scribus.PAPER_A4
scribus.newDocument(scribus.PAPER_A4, (margin,) * 4, scribus.PORTRAIT, 1, scribus.UNIT_POINTS, scribus.PAGE_1, 0, 1)
frame = scribus.createText(margin, margin, width - 2 * margin, height - 2 * margin)
scribus.setText("\r".join(paragraphs), frame)
scribus.saveDocAs(output)
