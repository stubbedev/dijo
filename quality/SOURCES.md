# Sources of the ground truth

The texts in `truth/` named after a language that the rest of the corpus does not use (`albanian.txt` to `ukrainian.txt`, other
than `danish`, `swedish`, `norwegian`, `german`, `polish`, `czech`, `turkish`, `greek`, `russian` and the scripts of
the `script` category) are the title, the preamble and the first articles, about 200 words, of each language's
translation of the Universal Declaration of Human Rights, as collected by the
[UDHR in Unicode](https://github.com/eric-muller/udhr) project (`data/udhr/udhr_<code>.xml`). Spaces before commas
and full stops are removed and U+2010 hyphens are written as `-`; the text is otherwise unchanged. Serbian is in
Cyrillic (`serbian`) and Latin (`serbian-latin`), Norwegian in Bokmål (`norwegian`) and Nynorsk (`nynorsk`).

`quixote-1605.pdf` is pages 30, 200, 423 and 450 of the Internet Archive scan
[elingeniosohidal00cerguat](https://archive.org/details/elingeniosohidal00cerguat) of *El ingenioso hidalgo don Quixote
de la Mancha* (Madrid, Juan de la Cuesta, 1605), its page images unchanged and its OCR text layer removed, so that every
page is read by OCR: early modern type, worn and uneven, with long s, `ç` and abbreviations marked by a tilde. Page 423
sets a sonnet above prose. `truth/quixote.txt` is a hand transcription of the four pages' body text, without the
running heads, page numbers and catchwords: long s is written `s`, abbreviations (`q̃`, `cõ`, `quiẽ`), accents, `u`
for `v` and the printed spelling are kept, and words hyphenated across lines are joined.

`arbeiter-zeitung-1895.jpg` is page 4 of the *Arbeiter-Zeitung* (Vienna) of 6 July 1895 and `neue-freie-presse-1895.tif`
page 10 of the *Neue Freie Presse* of the same day, `ONB_aze_18950706_4` and `ONB_nfp_18950706_010` of the validation
set of [UB-Mannheim/AustrianNewspapers](https://github.com/UB-Mannheim/AustrianNewspapers) 2.0.0
(`data/ValidationSet_ONB_Newseye_GT_M1+/GT-PAGE`, commit `92cd4e51`), the NewsEye / READ OCR training dataset from
Austrian Newspapers of the Austrian National Library (Mühlberger & Hackl, 2019,
[doi:10.5281/zenodo.3387369](https://doi.org/10.5281/zenodo.3387369)) as revised by Mannheim University Library,
published under the [Creative Commons Attribution 4.0 International license](https://creativecommons.org/licenses/by/4.0/).
The images are unchanged: newspaper columns set in blackletter (Fraktur) with long s, the first a greyscale scan, the
second a bilevel one with tables of stock prices set in roman type. `truth/arbeiter-zeitung-1895.txt` and
`truth/neue-freie-presse-1895.txt` join the `TextLine` transcriptions (`TextEquiv/Unicode`) of each page's regions in
the page's reading order, a line to a line and a region to a paragraph, without the running heads (the regions of type
`header`): long s is written `s` and the double hyphen `⸗` as `-`, as `truth/quixote.txt` writes them, and words
hyphenated across lines are joined.

## Real scans with published ground truth

These pages come from datasets and archives whose licences allow redistribution; each image is unchanged unless said
otherwise. As for the Quixote, running heads, page numbers, signatures and catchwords are left out of the truth, long
s is written `s`, abbreviations and the printed spelling are kept, and words hyphenated across lines are joined, but a
word split across lines without a hyphen is left split as printed. A word hyphenated across the foot of the page ends
with its hyphen. Corrections to the published transcription, checked against the image, are listed per page.

- `more-epigrammata-1520.jpg`: Thomas More, *Epigrammata* (Basel, 1520), from
  [HTR-United/cremma-16-17-print](https://github.com/HTR-United/cremma-16-17-print)
  `lat/e-rara-74397/15_3ab84_default` (repository CC0-1.0, catalogue entry CC-BY-4.0, by Thibault Clérice); the image
  is [e-rara-74397](https://doi.org/10.3931/e-rara-74397), Public Domain Mark 1.0. Truth from the ALTO
  `String@CONTENT`; the decorated initial V is not transcribed, as in the source; `Itaqꝫ`, missing from the source,
  is added.
- `zoologia-danica-1779.png`: O. F. Müller, *Zoologia Danica* (1779), p. 71, from the same repository,
  `lat/archiveorg-zoologiadanicase01mlle/zoologiadanicase01mlle.pdf_page_87`; the scan is Internet Archive
  [zoologiadanicase01mlle](https://archive.org/details/zoologiadanicase01mlle) (Smithsonian Libraries, "not in
  copyright"). The PNG is recompressed losslessly with oxipng. Corrected: `quinquefariam`, `ROND.`, `c.25`, `5.`,
  `Fem-raded Strand-Spröyter`, `indagatore`, `pentactem`.
- `unlv-8087-054.tif` and `unlv-8071-093.tif`: UNLV-ISRI magazine pages, *Travel & Leisure*, September 1993, p. 54,
  and a three-column article spread, p. 93, bilevel 300 dpi, from [tesseract-ocr/test](https://github.com/tesseract-ocr/test)
  `testing/8087_054.3B` and `testing/8071_093.3B` (Apache-2.0). Truth from the Latin-1 `.txt` in zone order: `~`
  marks are replaced by the printed characters (`Café`, `Dabelić`, `Mladić`, `©`, em and en dashes), the `~~` before
  `successful` and the footer and page number zones are dropped.
- `betrayed-armenia-1910-p46.tif` and `colonial-florida-1892-p14.tif`: pages of Diana Agabeg Apcar, *Betrayed Armenia*
  (Yokohama, 1910), and Richard L. Campbell, *Historical Sketches of Colonial Florida* (1892), from the `old_books`
  collection (scans of public domain Internet Archive books) of the
  [Noisy OCR Dataset](https://zenodo.org/records/5068735) (CC-BY-4.0, Thomas Hegghammer), set `02_bin` without added
  noise, files `a056` and `g020`. The bilevel TIFFs are recompressed losslessly to CCITT G4. Truth from
  `ground_truth/old_books_gt`; `Forida` is kept as printed.
- `arabic-wikipedia-dolphin-scan.png` and `arabic-wikipedia-village-scan.png`: printed and scanned Arabic Wikipedia
  articles (text CC-BY-SA) from the `yarmouk` collection of the same dataset, set `02_bin`, files `20066_1` and
  `11157_1`. The 8-bit RGB TIFFs, grey in every pixel, are converted losslessly to greyscale PNG. Truth from
  `ground_truth/yarmouk_gt`, unchanged.
- `hindi-diksita-1895-01.jpg` and `hindi-diksita-1895-02.jpg`: pages 01 and 02 of `diksita1895.zip` of
  [Ground Truth data for printed Devanagari](https://doi.org/10.11588/data/EGOKEI) (heiDATA, CC-BY-4.0, Nicole
  Merkel-Hilf, Heidelberg University Library): a Naval Kishore Press print of 1895, torn and cracked. The JPEGs are
  cropped losslessly with jpegtran to drop the library's digitisation footer. Truth from the ALTO `String@CONTENT`
  with the `(...)` marks of hidden text removed; corrected: `दोहा`, and `बहु` added before `बाँध्यो`.
- `japanese-ndl-1910s.jpg` and `japanese-ndl-1920s.jpg`: vertical two-page spreads `955662_R0000089` (遠藤無水,
  *社会主義者になつた漱石の猫*, 1910s; the author died in 1962) and `939618_R0000020` (石田新太郎, *成人教育施設案内*,
  1925; died 1927) of [ndl-lab/pdmocrdataset-part1](https://github.com/ndl-lab/pdmocrdataset-part1) (National Diet
  Library, Public Domain Mark 1.0), taken from `tosho_all_linejson.zip` by HTTP range request. Truth from the line
  JSON: ruby and page numbers are dropped and the lines are ordered by column, right to left and top to bottom, and
  joined without spaces; corrected: `共產黨宣言`, `廢滅`, `御參考`.
- `russian-vaska-1901.jpg`: page 9 of K. M. Stanyukovich, *Васька* (Vyatka, 1901), in pre-reform spelling with
  bleed-through, the page image extracted unchanged from the Commons scan
  [Васька (Станюкович, 1901).pdf](https://commons.wikimedia.org/wiki/File:Васька_(Станюкович,_1901).pdf)
  (PD-old-100). Truth from the validated Russian Wikisource page `Страница:Васька (Станюкович, 1901).pdf/9`
  (CC-BY-SA-4.0), with the footnote marker and dashes as printed.
- `armenian-dussap-1959-p28.png` and `armenian-dussap-1959-p29.png`: pages 28 and 29 of Srbuhi Dussap, *Երկեր*
  (Yerevan, 1959; the author died in 1901), djvu pages 50 and 51 of the Commons scan
  [Երկեր - Սրբուհի Տյուսաբ.djvu](https://commons.wikimedia.org/wiki/File:Երկեր_-_Սրբուհի_Տյուսաբ.djvu)
  (PD-old-100), each bilevel page rendered at its 300 dpi to PNG. Truth from the validated Armenian Wikisource pages
  `Էջ:Երկեր - Սրբուհի Տյուսաբ.djvu/50` and `/51` (CC-BY-SA-4.0), with the words split across the pages' feet put
  where they are printed.
