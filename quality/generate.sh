#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -lt 1 ]; then
    echo "usage: $0 <font package>..." >&2
    exit 2
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
truth="$here/truth"
corpus="$here/corpus"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

{
    echo '<?xml version="1.0"?><!DOCTYPE fontconfig SYSTEM "fonts.dtd"><fontconfig>'
    for package in "$@"; do
        echo "<dir>$package/share/fonts</dir>"
    done
    echo "<cachedir>$work/fontconfig</cachedir></fontconfig>"
} > "$work/fonts.conf"
export FONTCONFIG_FILE="$work/fonts.conf"
export HOME="$work/home"
mkdir -p "$HOME"

xml_escape() {
    sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g'
}


fodt() {
    local source="$1" output="$2" layout="$3" direction="$4" complex="${5:-Noto Sans}" asian="${6:-Noto Sans CJK SC}"
    local columns="" header_style="" header="" page_direction=""
    if [ "$layout" = vertical ]; then
        page_direction=' style:writing-mode="tb-rl"'
    fi
    if [ "$layout" = columns ]; then
        columns='<style:columns fo:column-count="2" fo:column-gap="1cm"/>'
    fi
    if [ "$layout" = running ]; then
        header_style='<style:header-style><style:header-footer-properties fo:min-height="0.6cm" fo:margin-bottom="0.4cm"/></style:header-style><style:footer-style><style:header-footer-properties fo:min-height="0.6cm" fo:margin-top="0.4cm"/></style:footer-style>'
        header='<style:header><text:p>Lighthouse Ledger Study</text:p></style:header><style:footer><text:p>Page <text:page-number text:select-page="current"/></text:p></style:footer>'
    fi
    {
        cat <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<office:document xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" office:version="1.3" office:mimetype="application/vnd.oasis.opendocument.text">
<office:styles>
<style:style style:name="Standard" style:family="paragraph"><style:paragraph-properties style:writing-mode="$direction" fo:margin-bottom="0.25cm"/><style:text-properties fo:font-size="11pt" style:font-name="Noto Sans" fo:font-family="Noto Sans" style:font-family-asian="$asian" style:font-family-complex="$complex"/></style:style>
<style:style style:name="Heading" style:family="paragraph" style:parent-style-name="Standard"><style:text-properties fo:font-size="16pt" fo:font-weight="bold"/></style:style>
</office:styles>
<office:automatic-styles>
<style:style style:name="Columns" style:family="section"><style:section-properties><style:columns fo:column-count="3" fo:column-gap="0.8cm"/></style:section-properties></style:style>
<style:page-layout style:name="Layout"><style:page-layout-properties fo:page-width="21cm" fo:page-height="29.7cm" fo:margin="2cm"$page_direction>$columns</style:page-layout-properties>$header_style</style:page-layout>
</office:automatic-styles>
<office:master-styles><style:master-page style:name="Standard" style:page-layout-name="Layout">$header</style:master-page></office:master-styles>
<office:body><office:text>
EOF
        local first=true count=0 total
        total=$(awk 'NF' "$source" | wc -l)
        while IFS= read -r line; do
            [ -z "$line" ] && continue
            count=$((count + 1))
            escaped=$(printf '%s' "$line" | xml_escape)
            if [ "$layout" = newspaper ] && [ "$count" -eq "$total" ]; then
                echo '</text:section>'
            fi
            if $first || [[ "$line" =~ ^Part\ [A-Z][a-z]+$ ]]; then
                first=false
                echo "<text:h text:style-name=\"Heading\" text:outline-level=\"1\">$escaped</text:h>"
            else
                echo "<text:p text:style-name=\"Standard\">$escaped</text:p>"
            fi
            if [ "$layout" = newspaper ] && [ "$count" -eq 1 ]; then
                echo '<text:section text:style-name="Columns" text:name="Columns">'
            fi
        done < "$source"
        echo '</office:text></office:body></office:document>'
    } > "$output"
}

convert() {
    local input="$1" name="$2" extension="$3" filter="$4"
    soffice --headless --convert-to "$filter" --outdir "$work/out" "$input" > /dev/null
    mv "$work/out/$(basename "${input%.*}").$extension" "$corpus/$name.$extension"
}

rm -rf "$corpus"
mkdir -p "$corpus" "$work/out"

fodt "$truth/prose.txt" "$work/prose.fodt" plain lr-tb
convert "$work/prose.fodt" prose odt odt
convert "$work/prose.fodt" prose docx "docx:MS Word 2007 XML"
convert "$work/prose.fodt" prose doc "doc:MS Word 97"
convert "$work/prose.fodt" prose rtf rtf
convert "$work/prose.fodt" prose pdf pdf

awk 'NF' "$truth/prose.txt" | sed 's/^/\n/' > "$work/prose.md"
pandoc "$work/prose.md" -o "$corpus/prose-pandoc.docx"
pandoc "$work/prose.md" --standalone --metadata title=Ledger -o "$corpus/prose.html"
pandoc "$work/prose.md" --metadata title=Ledger -o "$corpus/prose.epub"

fodt "$truth/prose.txt" "$work/prose-columns.fodt" columns lr-tb
convert "$work/prose-columns.fodt" prose-columns pdf pdf
fodt "$truth/prose.txt" "$work/prose-newspaper.fodt" newspaper lr-tb
convert "$work/prose-newspaper.fodt" prose-newspaper pdf pdf

fodt "$truth/report.txt" "$work/report.fodt" running lr-tb
convert "$work/report.fodt" report pdf pdf
convert "$work/report.fodt" report docx "docx:MS Word 2007 XML"

european=(german danish swedish norwegian nynorsk polish czech turkish greek russian albanian armenian
    azerbaijani belarusian bosnian bulgarian catalan croatian dutch estonian faroese finnish french
    georgian hungarian icelandic irish italian latvian lithuanian luxembourgish macedonian maltese
    portuguese romanian romansh serbian serbian-latin slovak slovenian spanish ukrainian)

for language in "${european[@]}" arabic hebrew chinese japanese thai hindi; do
    direction=lr-tb
    complex="Noto Sans"
    asian="Noto Sans CJK SC"
    case "$language" in
        arabic) direction=rl-tb complex="Noto Naskh Arabic" ;;
        hebrew) direction=rl-tb complex="Noto Sans Hebrew" ;;
        thai) complex="Noto Sans Thai" ;;
        hindi) complex="Noto Sans Devanagari" ;;
        japanese) asian="Noto Sans CJK JP" ;;
    esac
    fodt "$truth/$language.txt" "$work/$language.fodt" plain "$direction" "$complex" "$asian"
    convert "$work/$language.fodt" "$language" pdf pdf
    convert "$work/$language.fodt" "$language" docx "docx:MS Word 2007 XML"
done

fodt "$truth/japanese.txt" "$work/japanese-vertical.fodt" vertical page "Noto Sans" "Noto Sans CJK JP"
convert "$work/japanese-vertical.fodt" japanese-vertical pdf \
    'pdf:writer_pdf_Export:{"UseTaggedPDF":{"type":"boolean","value":"false"}}'

for extension in odt docx doc pdf; do
    case "$extension" in
        docx) filter="docx:MS Word 2007 XML" ;;
        doc) filter="doc:MS Word 97" ;;
        *) filter=$extension ;;
    esac
    convert "$here/notes.fodt" notes "$extension" "$filter"
done

convert "$here/revisions.fodt" revisions odt odt
convert "$here/revisions.fodt" revisions docx "docx:MS Word 2007 XML"
convert "$here/revisions.fodt" revisions doc "doc:MS Word 97"

tr '\t' ',' < "$truth/table.txt" > "$work/table.csv"
convert "$work/table.csv" table xlsx xlsx
{
    head -n 1 "$truth/table.txt" | sed 's/\t/ | /g; s/^/| /; s/$/ |/'
    head -n 1 "$truth/table.txt" | sed 's/[^\t]*/---/g; s/\t/ | /g; s/^/| /; s/$/ |/'
    tail -n +2 "$truth/table.txt" | sed 's/\t/ | /g; s/^/| /; s/$/ |/'
} > "$work/table.md"
pandoc "$work/table.md" -o "$corpus/table.docx"
pandoc "$work/table.md" --standalone --metadata title=Table -o "$corpus/table.html"
convert "$corpus/table.docx" table pdf pdf

while read -r name encoding; do
    iconv -f UTF-8 -t "$encoding" "$truth/$name.txt" > "$corpus/$name-${encoding,,}.txt"
done < "$here/legacy-encodings.tsv"

awk 'NF' "$truth/hyphenated.txt" | sed 's/^/\n/' > "$work/hyphenated.md"
pandoc "$work/hyphenated.md" -o "$corpus/hyphenated.pdf" --pdf-engine=pdflatex -V lang=en-US \
    -V classoption=twocolumn -V geometry:paperwidth=12cm,paperheight=29.7cm,margin=1cm

for mapping in private-use offset; do
    python3 "$here/text_pdf.py" "$truth/prose.txt" "$corpus/prose-broken-$mapping.pdf" "$mapping" lines
done
python3 "$here/text_pdf.py" "$truth/prose.txt" "$corpus/prose-columns-reversed.pdf" intact reversed-columns

pdftoppm -r 300 -f 1 -l 1 -png -singlefile "$corpus/prose.pdf" "$work/scan-300"
pdftoppm -r 150 -f 1 -l 1 -png -singlefile "$corpus/prose.pdf" "$work/scan-150"
magick "$work/scan-300.png" -colorspace Gray -depth 8 "$corpus/prose-scan-300dpi.png"
magick "$work/scan-150.png" -colorspace Gray -depth 8 "$corpus/prose-scan-150dpi.png"
magick "$work/scan-300.png" -colorspace Gray -background white -rotate 2.5 -depth 8 "$corpus/prose-scan-skewed.png"
magick "$work/scan-300.png" -colorspace Gray -rotate 90 -depth 8 "$corpus/prose-scan-rotated.png"
magick "$work/scan-300.png" -colorspace Gray -rotate 180 -depth 8 "$corpus/prose-scan-upside-down.png"
magick "$work/scan-300.png" -colorspace Gray -depth 8 -density 300 -compress jpeg -quality 85 "$corpus/prose-scan.pdf"
magick "$work/scan-300.png" -colorspace Gray -negate -depth 8 "$corpus/prose-scan-inverted.png"
magick "$work/scan-300.png" -colorspace Gray -quality 90 "$corpus/prose-scan.webp"
magick "$work/scan-300.png" -colorspace Gray -quality 90 "$corpus/prose-scan.heic"
magick "$work/scan-300.png" -colorspace Gray -quality 90 "$corpus/prose-scan.avif"
magick "$work/scan-300.png" -colorspace Gray -threshold 50% "$corpus/prose-scan.pbm"
cjb2 -dpi 300 "$corpus/prose-scan.pbm" "$corpus/prose-scan.djvu"
jbig2 "$corpus/prose-scan.pbm" > "$corpus/prose-scan.jb2"
magick "$work/scan-300.png" -colorspace Gray -depth 8 "$work/scan-300.pgm"
opj_compress -i "$work/scan-300.pgm" -o "$corpus/prose-scan.jp2" -r 30
magick "$work/scan-300.png" -colorspace Gray -depth 8 "$corpus/prose-scan.psd"
magick "$work/scan-300.png" -colorspace Gray -type Palette "$corpus/prose-scan.pcx"
magick "$work/scan-300.png" -colorspace Gray -depth 8 -compress RLE "$corpus/prose-scan.tga"
magick "$work/scan-300.png" -colorspace Gray \( -size 2480x3508 radial-gradient:white-gray20 \) -compose multiply \
    -composite -depth 8 "$corpus/prose-scan-shadow.png"
magick "$work/scan-300.png" -colorspace Gray \( -size 2480x3508 xc:white -fill gray30 \
    -draw 'polygon 1600,0 2480,0 2480,3508 500,3508' -blur 0x6 \) -compose multiply -composite \
    \( -size 2480x3508 radial-gradient:white-gray55 \) -compose multiply -composite -depth 8 \
    "$corpus/prose-photo-shadow.png"
for language in "${european[@]}"; do
    pdftoppm -r 300 -f 1 -l 1 -png -singlefile "$corpus/$language.pdf" "$work/$language-scan"
    magick "$work/$language-scan.png" -colorspace Gray -depth 8 "$corpus/$language-scan.png"
done
for language in chinese japanese thai hindi hebrew; do
    pdftoppm -r 300 -f 1 -l 1 -png -singlefile "$corpus/$language.pdf" "$work/$language-scan"
    magick "$work/$language-scan.png" -colorspace Gray -depth 8 "$corpus/$language-scan.png"
done
for language in danish polish; do
    magick "$corpus/$language-scan.png" -resize 50% -seed 1 -attenuate 0.3 +noise Impulse -threshold 55% \
        "$corpus/$language-scan-fax.png"
done
magick "$corpus/danish-scan.png" \( "$corpus/french-scan.png" -flop -fill white -colorize 82% \) \
    -compose multiply -composite -depth 8 "$corpus/danish-scan-bleed.png"
pdftoppm -r 72 -f 1 -l 1 -png -singlefile "$corpus/prose.pdf" "$work/scan-72"
magick "$work/scan-72.png" -colorspace Gray -depth 8 "$corpus/prose-scan-72dpi.png"
pdftoppm -r 100 -f 1 -l 1 -png -singlefile "$corpus/prose.pdf" "$work/scan-100"
magick "$work/scan-100.png" -colorspace Gray -blur 0x0.5 -seed 1 -attenuate 1.5 +noise Gaussian -threshold 55% \
    -depth 8 "$corpus/prose-scan-100dpi-degraded.png"
pdftoppm -r 600 -f 1 -l 1 -png -singlefile "$corpus/prose.pdf" "$work/scan-600"
magick "$work/scan-600.png" -colorspace Gray -depth 8 "$corpus/prose-scan-600dpi.png"
pdftoppm -r 200 -png "$corpus/report.pdf" "$work/report-scan"
magick "$work"/report-scan-*.png -colorspace Gray -depth 8 -compress lzw "$corpus/report-scan.tif"
magick "$corpus/report-scan.tif" -background white -rotate 2 -depth 8 -compress lzw "$corpus/report-scan-skewed.tif"
magick "$work/scan-300.png" -colorspace Gray -rotate 180 -quality 90 "$corpus/prose-photo-upside-down.jpg"
exiftool -quiet -overwrite_original -Orientation=3 -n "$corpus/prose-photo-upside-down.jpg"
magick "$work/scan-150.png" -colorspace Gray -resize 1240x \( +clone -fill '#555555' -colorize 100 -resize 140% \) \
    +swap -gravity center -compose over -composite -virtual-pixel background -background '#555555' \
    -distort Perspective '0,0 420,260  1736,0 1330,300  1736,2455 1700,2380  0,2455 40,2420' -quality 90 \
    "$corpus/prose-photo-perspective.jpg"
magick "$work/scan-150.png" -colorspace Gray -virtual-pixel white \
    -fx '(0.55 + 0.45*i/w) * u.p{i + 45*pow(1-i/w,3), j - 110*pow(1-i/w,3) + 50*pow(2*i/w-1,2)*(j/h)}' \
    -quality 90 "$corpus/prose-photo-curled.jpg"
for layout in columns newspaper; do
    pdftoppm -r 300 -f 1 -l 1 -png -singlefile "$corpus/prose-$layout.pdf" "$work/$layout-300"
    magick "$work/$layout-300.png" -colorspace Gray -depth 8 "$corpus/prose-$layout-scan.png"
done
python3 "$here/typewritten.py" "$truth/prose.txt" "$work/prose-typewritten.txt"
magick -size 2480x3508 xc:white -density 300 -font Courier-Prime-Regular -pointsize 12 -fill black \
    -annotate +310+350 @"$work/prose-typewritten.txt" -colorspace Gray -alpha off -blur 0x1 -seed 1 \
    -attenuate 1.5 +noise Gaussian -threshold 55% -depth 8 "$corpus/prose-typewriter-scan.png"
pdftoppm -r 300 -f 1 -l 1 -png -singlefile "$corpus/arabic.pdf" "$work/arabic-300"
magick "$work/arabic-300.png" -colorspace Gray -depth 8 "$corpus/arabic-scan-300dpi.png"
cp "$here/quixote-1605.pdf" "$corpus/quixote-1605.pdf"
