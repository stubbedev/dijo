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
    local columns="" header_style="" header=""
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
<style:page-layout style:name="Layout"><style:page-layout-properties fo:page-width="21cm" fo:page-height="29.7cm" fo:margin="2cm">$columns</style:page-layout-properties>$header_style</style:page-layout>
</office:automatic-styles>
<office:master-styles><style:master-page style:name="Standard" style:page-layout-name="Layout">$header</style:master-page></office:master-styles>
<office:body><office:text>
EOF
        local first=true
        while IFS= read -r line; do
            [ -z "$line" ] && continue
            escaped=$(printf '%s' "$line" | xml_escape)
            if $first || [[ "$line" =~ ^Part\ [A-Z][a-z]+$ ]]; then
                first=false
                echo "<text:h text:style-name=\"Heading\" text:outline-level=\"1\">$escaped</text:h>"
            else
                echo "<text:p text:style-name=\"Standard\">$escaped</text:p>"
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

fodt "$truth/report.txt" "$work/report.fodt" running lr-tb
convert "$work/report.fodt" report pdf pdf
convert "$work/report.fodt" report docx "docx:MS Word 2007 XML"

for language in german arabic hebrew chinese japanese thai hindi; do
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
    python3 "$here/broken_text_layer.py" "$truth/prose.txt" "$corpus/prose-broken-$mapping.pdf" "$mapping"
done

pdftoppm -r 300 -f 1 -l 1 -png -singlefile "$corpus/prose.pdf" "$work/scan-300"
pdftoppm -r 150 -f 1 -l 1 -png -singlefile "$corpus/prose.pdf" "$work/scan-150"
magick "$work/scan-300.png" -colorspace Gray -depth 8 "$corpus/prose-scan-300dpi.png"
magick "$work/scan-150.png" -colorspace Gray -depth 8 "$corpus/prose-scan-150dpi.png"
magick "$work/scan-300.png" -colorspace Gray -background white -rotate 2.5 -depth 8 "$corpus/prose-scan-skewed.png"
magick "$work/scan-300.png" -colorspace Gray -rotate 90 -depth 8 "$corpus/prose-scan-rotated.png"
magick "$work/scan-300.png" -colorspace Gray -depth 8 -density 300 -compress jpeg -quality 85 "$corpus/prose-scan.pdf"
magick "$work/scan-300.png" -colorspace Gray -negate -depth 8 "$corpus/prose-scan-inverted.png"
magick "$work/scan-300.png" -colorspace Gray \( -size 2480x3508 radial-gradient:white-gray20 \) -compose multiply \
    -composite -depth 8 "$corpus/prose-scan-shadow.png"
pdftoppm -r 72 -f 1 -l 1 -png -singlefile "$corpus/prose.pdf" "$work/scan-72"
magick "$work/scan-72.png" -colorspace Gray -depth 8 "$corpus/prose-scan-72dpi.png"
pdftoppm -r 200 -png "$corpus/report.pdf" "$work/report-scan"
magick "$work"/report-scan-*.png -colorspace Gray -depth 8 -compress lzw "$corpus/report-scan.tif"
magick "$corpus/report-scan.tif" -background white -rotate 2 -depth 8 -compress lzw "$corpus/report-scan-skewed.tif"
magick "$work/scan-300.png" -colorspace Gray -rotate 180 -quality 90 "$corpus/prose-photo-upside-down.jpg"
exiftool -quiet -overwrite_original -Orientation=3 -n "$corpus/prose-photo-upside-down.jpg"
pdftoppm -r 300 -f 1 -l 1 -png -singlefile "$corpus/arabic.pdf" "$work/arabic-300"
magick "$work/arabic-300.png" -colorspace Gray -depth 8 "$corpus/arabic-scan-300dpi.png"
