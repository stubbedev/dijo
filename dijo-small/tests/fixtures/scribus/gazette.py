"""Writes test_files/documents/gazette.sla, a Scribus document in the SCRIBUSUTF8NEW format
Scribus 1.5.8 saves, with what ScribusParser reads and what it leaves out:

- the document information (TITLE, AUTHOR, SUBJECT, COMMENTS, KEYWORDS, PUBLISHER, DOCDATE,
  DOCLANGINFO) and the text language (LANGUAGE) on <DOCUMENT>;
- paragraph styles, one based on a heading's;
- a master page with a running head frame, two pages and a frame on the pasteboard, the page
  items saved in an order of their own (Scribus saves them in their stacking order) rather than
  in the order they are read in;
- a story threaded from a frame on the first page to one on the second, its text saved in the
  first frame only, with a heading, a tab, a hard space, a line break and a point Scribus marked
  to hyphenate at;
- an image kept in the document (base 64 of Qt's qCompress of a white 4x2 PNG), in a frame
  anchored in the story's text;
- a group of a caption frame and a shape, and a table of three rows and two columns.

Usage: gazette.py OUTPUT.sla
"""

import base64
import importlib.util
import pathlib
import struct
import sys
import zlib


def png(width, height):
    """The white PNG abiword/harbour.py writes, `width` by `height` pixels."""
    path = pathlib.Path(__file__).resolve().parent.parent / "abiword" / "harbour.py"
    spec = importlib.util.spec_from_file_location("harbour", path)
    harbour = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(harbour)
    return harbour.png(width, height)


def q_compress(data):
    """`data` as Qt's qCompress compresses it: its length, big-endian, and its zlib stream."""
    return struct.pack(">I", len(data)) + zlib.compress(data, 9)


ITEM = (
    'XPOS="{x}" YPOS="{y}" OwnPage="{page}" ItemID="{id}" PTYPE="{ptype}" WIDTH="{width}" '
    'HEIGHT="{height}" FRTYPE="0" CLIPEDIT="0" PWIDTH="1" PLINEART="1" ANNAME="{name}" '
    'LOCALSCX="1" LOCALSCY="1" LOCALX="0" LOCALY="0" LOCALROT="0" PICART="1" SCALETYPE="1" '
    'RATIO="1" COLUMNS="1" COLGAP="0" AUTOTEXT="0" EXTRA="0" TEXTRA="0" BEXTRA="0" REXTRA="0" '
    'VAlign="0" FLOP="0" PLTSHOW="0" BASEOF="0" textPathType="0" textPathFlipped="0" '
    'path="M0 0 L{width} 0 L{width} {height} L0 {height} L0 0 Z" gXpos="0" gYpos="0" '
    'gWidth="0" gHeight="0" LAYER="0"'
)


def item(element, x, y, page, id, ptype, name, width=200, height=100, extra="", body=""):
    """A page item `element` of type `ptype`, with the attributes `extra` and the content
    `body`."""
    attributes = ITEM.format(
        x=x, y=y, page=page, id=id, ptype=ptype, width=width, height=height, name=name
    )
    if extra:
        attributes += " " + extra
    if not body:
        return "\t\t<{} {}/>\n".format(element, attributes)
    return "\t\t<{0} {1}>\n{2}\t\t</{0}>\n".format(element, attributes, body)


def story(*elements):
    """A <StoryText> of `elements`."""
    lines = ["\t\t\t<StoryText>\n", "\t\t\t\t<DefaultStyle/>\n"]
    lines += ["\t\t\t\t{}\n".format(element) for element in elements]
    lines.append("\t\t\t</StoryText>\n")
    return "".join(lines)


def text_frame(element, x, y, page, id, name, elements, next=-1, back=-1, extra=""):
    """A text frame threaded on to the frame `next` and from the frame `back`."""
    links = 'NEXTITEM="{}" BACKITEM="{}"'.format(next, back)
    if extra:
        links += " " + extra
    body = story(*elements) if back == -1 else ""
    return item(element, x, y, page, id, 4, name, extra=links, body=body)


def cell(row, column, text):
    return (
        '\t\t\t\t<Cell Row="{}" Column="{}" Style="" TextColumns="1" TextColGap="0" '
        'TextDistLeft="0" TextDistTop="0" TextDistBottom="0" TextDistRight="0" '
        'TextVertAlign="0" Flop="0" FillColor="None" FillShade="100" LeftPadding="1" '
        'RightPadding="1" TopPadding="1" BottomPadding="1">\n'
        "\t\t\t\t\t<StoryText>\n\t\t\t\t\t\t<DefaultStyle/>\n"
        '\t\t\t\t\t\t<ITEXT CH="{}"/>\n\t\t\t\t\t\t<trail/>\n'
        "\t\t\t\t\t</StoryText>\n\t\t\t\t</Cell>\n"
    ).format(row, column, text)


PAGE = (
    '\t\t<{element} PAGEXPOS="100" PAGEYPOS="{y}" PAGEWIDTH="595.275590551181" '
    'PAGEHEIGHT="841.889763779528" BORDERLEFT="40" BORDERRIGHT="40" BORDERTOP="40" '
    'BORDERBOTTOM="40" NUM="{num}" NAM="{name}" MNAM="{master}" Size="A4" Orientation="0" '
    'LEFT="0" PRESET="0" VerticalGuides="" HorizontalGuides="" AGhorizontalAutoGap="0" '
    'AGverticalAutoGap="0" AGhorizontalAutoCount="0" AGverticalAutoCount="0" '
    'AGhorizontalAutoRefer="0" AGverticalAutoRefer="0" AGSelection="0 0 0 0" '
    'pageEffectDuration="1" pageViewDuration="1" effectType="0" Dm="0" M="0" Di="0"/>\n'
)


def document(image):
    master = PAGE.format(element="MASTERPAGE", y=20, num=0, name="Normal", master="")
    pages = PAGE.format(element="PAGE", y=20, num=0, name="", master="Normal") + PAGE.format(
        element="PAGE", y=901.889763779528, num=1, name="", master="Normal"
    )
    anchored = item(
        "FRAMEOBJECT",
        0,
        0,
        0,
        1817364523,
        2,
        "lamp-room",
        width=4,
        height=2,
        extra='InID="1" isInline="1" PFILE="" isInlineImage="1" inlineImageExt="png" '
        'ImageData="{}" NEXTITEM="-1" BACKITEM="-1"'.format(image),
    )
    running_head = text_frame(
        "MASTEROBJECT",
        140,
        30,
        0,
        402155377,
        "Running head",
        ['<ITEXT CH="The Harbour Gazette, issue 12"/>', "<trail/>"],
    )
    pasteboard = text_frame(
        "PAGEOBJECT",
        -180,
        200,
        -1,
        1554126630,
        "Draft note",
        ['<ITEXT CH="Draft: check the tide times with the harbour master."/>', "<trail/>"],
    )
    notices = text_frame(
        "PAGEOBJECT",
        140,
        1201.889763779528,
        1,
        97355184,
        "Notices",
        [
            '<ITEXT CH="Notices"/>',
            '<para PARENT="Section Head"/>',
            '<ITEXT CH="The fish market opens at five."/>',
            "<trail/>",
        ],
    )
    cells = (
        "\t\t\t<TableData Style=\"\" FillColor=\"None\" FillShade=\"100\">\n"
        + cell(0, 0, "Tide")
        + cell(0, 1, "Time")
        + cell(1, 0, "High tide")
        + cell(1, 1, "06:12")
        + cell(2, 0, "Low tide")
        + cell(2, 1, "12:31")
        + "\t\t\t</TableData>\n"
    )
    table = item(
        "PAGEOBJECT",
        140,
        700,
        0,
        1290512077,
        16,
        "Tides",
        width=300,
        height=90,
        extra='Rows="3" Columns="2" RowPositions="0 30 60" RowHeights="30 30 30" '
        'ColumnPositions="0 150" ColumnWidths="150 150" CellAreas="" TableStyle=""',
        body=cells,
    )
    continuation = text_frame(
        "PAGEOBJECT", 140, 941.889763779528, 1, 715203367, "Article 2", [], back=1460871933
    )
    caption = text_frame(
        "PAGEOBJECT",
        140,
        640,
        0,
        1963217854,
        "Caption",
        ['<ITEXT CH="Photo: Anna Berg"/>', "<trail/>"],
    )
    rule = item("PAGEOBJECT", 140, 630, 0, 342986112, 6, "Rule", width=300, height=2)
    group = item(
        "PAGEOBJECT",
        140,
        630,
        0,
        2004163721,
        12,
        "Picture group",
        width=300,
        height=40,
        extra='groupWidth="300" groupHeight="40"',
        body=caption + rule,
    )
    article = text_frame(
        "PAGEOBJECT",
        140,
        160,
        0,
        1460871933,
        "Article",
        [
            '<ITEXT CH="Storm warning"/>',
            '<para PARENT="Section Head"/>',
            '<ITEXT CH="Gales are expected from the north-west on Thursday. The light­house '
            'keeper asks all boats to stay in harbour."/>',
            "<para/>",
            '<ITEXT CH="Moorings"/>',
            "<tab/>",
            '<ITEXT CH="Pier"/>',
            "<nbspace/>",
            '<ITEXT CH="3"/>',
            "<para/>",
            '<ITEXT CH="Harbour Office"/>',
            "<breakline/>",
            '<ITEXT CH="Quay Street 1"/>',
            "<para/>",
            '<ITEXT CH="The lamp room after repairs"/>',
            '<ITEXT Unicode="25" COBJ="1"/>',
            '<ITEXT CH="."/>',
            "<para/>",
            '<ITEXT CH="Repairs continue next week."/>',
            '<var name="pgno"/>',
            "<trail/>",
        ],
        next=715203367,
    )
    title = text_frame(
        "PAGEOBJECT",
        140,
        60,
        0,
        1271309245,
        "Title",
        ['<ITEXT CH="The Harbour Gazette"/>', '<trail PARENT="Heading 1"/>'],
    )
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<SCRIBUSUTF8NEW Version="1.5.8">\n'
        '\t<DOCUMENT ANZPAGES="2" PAGEWIDTH="595.275590551181" PAGEHEIGHT="841.889763779528" '
        'BORDERLEFT="40" BORDERRIGHT="40" BORDERTOP="40" BORDERBOTTOM="40" PRESET="0" '
        'BleedTop="0" BleedLeft="0" BleedRight="0" BleedBottom="0" ORIENTATION="0" '
        'PAGESIZE="A4" FIRSTNUM="1" BOOK="0" AUTOSPALTEN="1" ABSTSPALTEN="11" UNITS="1" '
        'DFONT="DejaVu Sans Book" DSIZE="12" DCOL="1" DGAP="0" TabFill="" TabWidth="36" '
        'TextDistLeft="0" TextDistRight="0" TextDistBottom="0" TextDistTop="0" '
        'FirstLineOffset="1" AUTHOR="Anna Berg" COMMENTS="Kept by the harbour office.&#10;'
        'Printed weekly." KEYWORDS="harbour, storm, tides" PUBLISHER="Harbour Office" '
        'DOCDATE="2026-11-02" DOCTYPE="Text" DOCFORMAT="" DOCIDENT="" DOCSOURCE="" '
        'DOCLANGINFO="en-GB" DOCRELATION="" DOCCOVER="" DOCRIGHTS="" DOCCONTRIB="" '
        'TITLE="The Harbour Gazette" SUBJECT="Storm warning and tide tables" VHOCH="33" '
        'VHOCHSC="66" VTIEF="33" VTIEFSC="66" VKAPIT="75" BASEGRID="14.4" BASEO="0" AUTOL="100" '
        'UnderlinePos="-1" UnderlineWidth="-1" StrikeThruPos="-1" StrikeThruWidth="-1" '
        'GROUPC="2" HCMS="0" DPSo="0" DPSFo="0" DPuse="0" DPgam="0" DPbla="1" DPPr="" DPIn="" '
        'DPInCMYK="" DPIn2="" DPIn3="" DISc="1" DIIm="0" ALAYER="0" LANGUAGE="en_GB" '
        'AUTOMATIC="1" AUTOCHECK="0" GUIDELOCK="0" SnapToGuides="0" SnapToGrid="0" '
        'SnapToElement="0" minGrid="20" majGrid="100" SHOWGRID="0" SHOWGUIDES="1" '
        'showcolborders="1" SHOWFRAME="1" SHOWControl="0" SHOWLAYERM="0" SHOWMARGIN="1" '
        'SHOWBASE="0" SHOWPICT="1" SHOWLINK="0" rulerMode="1" showrulers="1" '
        'showBleed="1" rulerXoffset="0" rulerYoffset="0" GapHorizontal="0" GapVertical="40" '
        'ScratchBottom="20" ScratchLeft="100" ScratchRight="100" ScratchTop="20" '
        'BACKG="#ffffff" PAGEC="#ffffff" MARGC="#0000ff" RANDF="0" currentProfile="PDF/X-3">\n'
        '\t\t<CheckProfile Name="PDF/X-3" ignoreErrors="0" autoCheck="1" checkGlyphs="1" '
        'checkOrphans="1" checkOverflow="1" checkPictures="1" checkPartFilledImageFrames="0" '
        'checkResolution="1" checkTransparency="1" minResolution="144" maxResolution="2400" '
        'checkAnnotations="1" checkRasterPDF="1" checkForGIF="1" ignoreOffLayers="0" '
        'checkNotCMYKOrSpot="0" checkDeviceColorsAndOutputIntent="1" checkFontNotEmbedded="1" '
        'checkFontIsOpenType="1" checkAppliedMasterDifferentSide="1" checkEmptyTextFrames="1"/>\n'
        '\t\t<COLOR NAME="Black" SPACE="CMYK" C="0" M="0" Y="0" K="100"/>\n'
        '\t\t<COLOR NAME="White" SPACE="CMYK" C="0" M="0" Y="0" K="0"/>\n'
        '\t\t<STYLE NAME="Default Paragraph Style" DefaultStyle="1" ALIGN="0" DIRECTION="0" '
        'LINESPMode="0" LINESP="15" INDENT="0" RMARGIN="0" FIRST="0" VOR="0" NACH="0" '
        'ParagraphEffectOffset="0" DROP="0" DROPLIN="2" DROPDIST="0" Bullet="0" Numeration="0" '
        'HyphenConsecutiveLines="2" BCOLOR="None" BSHADE="100" FONT="DejaVu Sans Book" '
        'FONTSIZE="12" FEATURES="inherit" FCOLOR="Black" FSHADE="100" SCOLOR="Black" '
        'SSHADE="100" TXTSHX="5" TXTSHY="-5" TXTOUT="1" TXTULP="-0.1" TXTULW="-0.1" '
        'TXTSTP="-0.1" TXTSTW="-0.1" SCALEH="100" SCALEV="100" BASEO="0" KERN="0" '
        'LANGUAGE="en_GB"/>\n'
        '\t\t<STYLE NAME="Heading 1" PARENT="Default Paragraph Style" FONTSIZE="24"/>\n'
        '\t\t<STYLE NAME="Heading 2" PARENT="Default Paragraph Style" FONTSIZE="16"/>\n'
        '\t\t<STYLE NAME="Section Head" PARENT="Heading 2" FCOLOR="Black"/>\n'
        '\t\t<CHARSTYLE CNAME="Default Character Style" DefaultStyle="1" '
        'FONT="DejaVu Sans Book" FONTSIZE="12" FEATURES="inherit" FCOLOR="Black" FSHADE="100" '
        'HyphenWordMin="3" SCOLOR="Black" BGCOLOR="None" BGSHADE="100" SSHADE="100" '
        'TXTSHX="5" TXTSHY="-5" TXTOUT="1" TXTULP="-0.1" TXTULW="-0.1" TXTSTP="-0.1" '
        'TXTSTW="-0.1" SCALEH="100" SCALEV="100" BASEO="0" KERN="0" LANGUAGE="en_GB"/>\n'
        '\t\t<TableStyle NAME="Default Table Style" DefaultStyle="1" FillColor="None" '
        'FillShade="100"/>\n'
        '\t\t<CellStyle NAME="Default Cell Style" DefaultStyle="1" FillColor="None" '
        'FillShade="100" LeftPadding="1" RightPadding="1" TopPadding="1" BottomPadding="1"/>\n'
        '\t\t<LAYERS NUMMER="0" LEVEL="0" NAME="Background" SICHTBAR="1" DRUCKEN="1" '
        'EDIT="1" SELECT="0" FLOW="1" TRANS="1" BLEND="0" OUTL="0" LAYERC="#000000"/>\n'
        "\t\t<PageSets>\n"
        '\t\t\t<Set Name="Single Page" FirstPage="0" Rows="1" Columns="1"/>\n'
        "\t\t</PageSets>\n"
        "\t\t<Sections>\n"
        '\t\t\t<Section Number="0" Name="0" From="0" To="1" Type="Type_1_2_3" Start="1" '
        'Reversed="0" Active="1" FillChar="0" FieldWidth="0"/>\n'
        "\t\t</Sections>\n"
        + master
        + pages
        + anchored
        + running_head
        + pasteboard
        + notices
        + table
        + continuation
        + group
        + article
        + title
        + "\t</DOCUMENT>\n</SCRIBUSUTF8NEW>\n"
    )


if __name__ == "__main__":
    image = base64.b64encode(q_compress(png(4, 2))).decode("ascii")
    with open(sys.argv[1], "w", encoding="utf-8", newline="\n") as output:
        output.write(document(image))
