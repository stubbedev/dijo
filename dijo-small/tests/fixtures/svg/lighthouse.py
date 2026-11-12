"""Writes test_files/documents/lighthouse.svg, an SVG image as Inkscape 1.3 saves one, with what
SvgParser reads and what it leaves out:

- a DOCTYPE naming the external SVG 1.1 DTD, which is never fetched, and an internal entity;
- the root's <title> and <desc>, and Inkscape's RDF metadata, whose dc:creator is a cc:Agent
  with a dc:title of its own;
- a style sheet and a script, a path, and text in <defs> that a <use> shows;
- multi-line text of sodipodi:role="line" tspans, text on a path, superscripts shifted by dy,
  text in a link and in a <switch> of two languages, and a group's nested <title>;
- XHTML in a <foreignObject>;
- an <image> whose data: URI holds a PNG of the word LANTERN, drawn here in a 5x7 pixel font,
  for OCR to read.

Usage: lighthouse.py OUTPUT.svg
"""

import base64
import struct
import sys
import zlib

GLYPHS = {
    "L": ["X....", "X....", "X....", "X....", "X....", "X....", "XXXXX"],
    "A": [".XXX.", "X...X", "X...X", "XXXXX", "X...X", "X...X", "X...X"],
    "N": ["X...X", "XX..X", "X.X.X", "X..XX", "X...X", "X...X", "X...X"],
    "T": ["XXXXX", "..X..", "..X..", "..X..", "..X..", "..X..", "..X.."],
    "E": ["XXXXX", "X....", "X....", "XXXX.", "X....", "X....", "XXXXX"],
    "R": ["XXXX.", "X...X", "X...X", "XXXX.", "X.X..", "X..X.", "X...X"],
}
WORD = "LANTERN"
SCALE = 6
MARGIN = 24
ADVANCE = 7


def png(word):
    """A greyscale PNG of `word`, black on white."""
    width = 2 * MARGIN + (len(word) * ADVANCE - 2) * SCALE
    height = 2 * MARGIN + 7 * SCALE
    rows = []
    for y in range(height):
        row = bytearray(b"\xff" * width)
        glyph_row = (y - MARGIN) // SCALE
        if 0 <= y - MARGIN < 7 * SCALE:
            for index, letter in enumerate(word):
                for column, pixel in enumerate(GLYPHS[letter][glyph_row]):
                    if pixel == "X":
                        x = MARGIN + (index * ADVANCE + column) * SCALE
                        row[x : x + SCALE] = b"\x00" * SCALE
        rows.append(b"\x00" + bytes(row))

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    header = struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(b"".join(rows), 9))
        + chunk(b"IEND", b"")
    )


SVG = """<?xml version="1.0" encoding="UTF-8" standalone="no"?>
<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "http://www.w3.org/Graphics/SVG/1.1/DTD/svg11.dtd" [
  <!ENTITY keeper "harbour master">
]>
<!-- Created with Inkscape (http://www.inkscape.org/) -->

<svg
   width="210mm"
   height="148mm"
   viewBox="0 0 210 148"
   version="1.1"
   id="svg1"
   inkscape:version="1.3.2 (091e20e, 2023-11-25)"
   sodipodi:docname="lighthouse.svg"
   xmlns:inkscape="http://www.inkscape.org/namespaces/inkscape"
   xmlns:sodipodi="http://sodipodi.sourceforge.net/DTD/sodipodi-0.dtd"
   xmlns:xlink="http://www.w3.org/1999/xlink"
   xmlns="http://www.w3.org/2000/svg"
   xmlns:svg="http://www.w3.org/2000/svg"
   xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"
   xmlns:cc="http://creativecommons.org/ns#"
   xmlns:dc="http://purl.org/dc/elements/1.1/">
  <title
     id="title1">The Lighthouse Keeper's Chart</title>
  <desc
     id="desc1">A chart of the harbour approaches,
     drawn for the keeper.</desc>
  <sodipodi:namedview
     id="namedview1"
     pagecolor="#ffffff"
     bordercolor="#000000"
     inkscape:zoom="0.7"
     inkscape:current-layer="layer1" />
  <defs
     id="defs1">
    <style
       id="style1">.label { font-family: serif; fill: #003366; }</style>
    <path
       id="coastline"
       d="M 20,120 C 60,100 120,100 190,120" />
    <text
       id="reef">Beware the reef</text>
  </defs>
  <metadata
     id="metadata1">
    <rdf:RDF>
      <cc:Work
         rdf:about="">
        <dc:format>image/svg+xml</dc:format>
        <dc:type
           rdf:resource="http://purl.org/dc/dcmitype/StillImage" />
        <dc:title>The Lighthouse Keeper's Chart</dc:title>
        <dc:creator>
          <cc:Agent>
            <dc:title>Anna Berg</dc:title>
          </cc:Agent>
        </dc:creator>
        <dc:subject>
          <rdf:Bag>
            <rdf:li>lighthouse</rdf:li>
            <rdf:li>chart</rdf:li>
          </rdf:Bag>
        </dc:subject>
        <dc:language>en</dc:language>
      </cc:Work>
    </rdf:RDF>
  </metadata>
  <script
     type="text/javascript"
     id="script1">document.title = "never shown";</script>
  <g
     inkscape:label="Layer 1"
     inkscape:groupmode="layer"
     id="layer1">
    <path
       style="fill:none;stroke:#000000;stroke-width:0.5"
       d="M 10,10 H 200 V 138 H 10 Z"
       id="frame" />
    <text
       xml:space="preserve"
       style="font-size:8px;line-height:1.25"
       x="20"
       y="30"
       id="text1"><tspan
         sodipodi:role="line"
         id="tspan1"
         x="20"
         y="30">Every evening at dusk</tspan><tspan
         sodipodi:role="line"
         x="20"
         y="40"
         id="tspan2">the keeper climbed the tower.</tspan></text>
    <text
       style="font-size:6px"
       id="text2"><textPath
         xlink:href="#coastline"
         id="textPath1">Along   the
         coast, the &keeper;&#x2019;s boats</textPath></text>
    <text
       x="20"
       y="60"
       id="text3">H<tspan
         dy="-3"
         style="font-size:4px">2</tspan><tspan
         dy="3">O and E = mc<tspan
           dy="-3">2</tspan></tspan></text>
    <a
       xlink:href="https://example.org/tides"
       id="link1"><text
         x="20"
         y="70"
         id="text4">Tide tables</text></a>
    <use
       xlink:href="#reef"
       x="20"
       y="80"
       id="use1" />
    <switch
       id="switch1">
      <text
         systemLanguage="da"
         x="20"
         y="90"
         id="text5">Fyret lyser om natten</text>
      <text
         x="20"
         y="90"
         id="text6">The light shines at night</text>
    </switch>
    <g
       id="wall"><title
         id="title2">Harbour wall</title><rect
         x="10"
         y="120"
         width="40"
         height="5"
         id="rect1" /></g>
    <foreignObject
       x="120"
       y="20"
       width="80"
       height="60"
       id="foreignObject1">
      <div
         xmlns="http://www.w3.org/1999/xhtml"><p>Lamp oil:
           <b>40</b> litres</p><style>p { color: red; }</style><ul><li>Wick trimmed</li><li>Lens polished</li></ul></div>
    </foreignObject>
    <image
       width="60"
       height="15"
       preserveAspectRatio="none"
       xlink:href="data:image/png;base64,{png}"
       id="image1"
       x="120"
       y="100" />
  </g>
</svg>
"""


def main():
    with open(sys.argv[1], "w", encoding="utf-8", newline="\n") as output:
        output.write(SVG.replace("{png}", base64.b64encode(png(WORD)).decode("ascii")))


if __name__ == "__main__":
    main()
