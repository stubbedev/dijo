# Sources of the ground truth

The texts in `truth/` named after a language that the rest of the corpus does not use (`albanian.txt` to
`ukrainian.txt`, other
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
published under the [Creative Commons Attribution 4.0 International
license](https://creativecommons.org/licenses/by/4.0/).
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
  and a three-column article spread, p. 93, bilevel 300 dpi, from
[tesseract-ocr/test](https://github.com/tesseract-ocr/test)
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
- `unlv-news-8229-025.tif`, `unlv-spanish-news-7601-039.tif`, `unlv-letter-8505-001.tif`, `unlv-legal-9464-015.tif`,
  `unlv-report-9500-023.tif`, `unlv-table-9521-052.tif` and `unlv-table-9560-036.tif`: pages `8229_025`, `7601_039`,
  `8505_001`, `9464_015`, `9500_023`, `9521_052` and `9560_036` of the UNLV-ISRI document collection's news, Spanish
  newspaper, business letter, legal and report samples, bilevel 300 dpi CCITT G4 TIFFs unchanged from `news.3B`,
  `spn.3B`, `bus.3B`, `legal.3B` and `rep.3B` of
  [isri-ocr-evaluation-tools](https://code.google.com/archive/p/isri-ocr-evaluation-tools) (Apache-2.0, as the
  project's licence states): a three-column Associated Press article of 1994, an Argentinian newspaper page with
  advertisements, a business letter of 1993, a page of a commercial lease, a two-column page of an annual report of
  1993 with a small table, and two financial tables from annual reports of 1993 (Lockheed, Monsanto), one set in
  old-style figures on a speckled scan. Truth from each page's Latin-1 `.txt`, kept a line to a line in zone order:
  running heads, page numbers and form numbers are left out, a blank line or, in the news, legal and Spanish pages,
  an indented line starts a paragraph, runs of spaces become one space, and words hyphenated across lines are joined
  unless the hyphen belongs to the word (`after-tax`). Additions checked against the image: the text of the two
  advertisements on `7601_039`, which the source leaves out, and `'My hands` for the source's `` `My hands``.
- `innsbrucker-nachrichten-1864.tif`, `neue-freie-presse-1873.tif` and `kronen-zeitung-1933.jpg`: page 6 of the
  *Innsbrucker Nachrichten* of 2 July 1864, page 10 of the *Neue Freie Presse* of 5 July 1873 and page 10 of the
  *Illustrierte Kronen-Zeitung* of 1 July 1933, `ONB_ibn_18640702_006`, `ONB_nfp_18730705_010` and
  `ONB_krz_19330701_010` of the same AustrianNewspapers validation set as the 1895 pages (CC-BY-4.0), images
  unchanged: Fraktur in one, three and four columns, the last a colour scan of a sports page mixing Fraktur and roman
  type with results tables and an advertisement. Truth as for the 1895 pages, except that a hyphen before a capital
  is kept (`Aeltesten-Collegium`), as printed.
- `polish-polanowski-1888.png`: page 80 of volume II of J. I. Kraszewski (d. 1887), *Adama Polanowskiego dworzanina
  króla JMci Jana III notatki* (Michał Glücksberg, 1888), 19th-century antiqua with yellowed paper, page curvature and
  the book edge visible, rendered locally from the DjVu at its native 300 dpi (1479×1947, PNG, recompressed losslessly
  with oxipng) from the Commons scan [Józef Ignacy Kraszewski - Adama Polanowskiego
  notatki.djvu](https://commons.wikimedia.org/wiki/File:J%C3%B3zef_Ignacy_Kraszewski_-_Adama_Polanowskiego_notatki.djvu)
  (PD-old-100-1923). Truth from the validated Polish Wikisource page `Strona:Józef Ignacy Kraszewski - Adama
  Polanowskiego notatki.djvu/268` (revision 1936999, CC-BY-SA-4.0), with the dashes restored as printed: Wikisource
  writes every dash as a spaced em dash, the print has unspaced em dashes in *pośpieszyć—ale*, *słuchałem—i*,
  *wymagał—niż*, *niemcami—choćby*, *mógł—bo* and a short spaced dash in *darmo – one*; printed oddities such as
  *zblizka*, *pomyśli* and the archaic *é* kept.
- `ukrainian-osnova-kulish-1861.png`: page 231 (PDF page 237) of P. O. Kulish (d. 1897, signing as Хуторянинъ), *Листи
  съ хутора. Листъ II*, in the journal *Основа* (St Petersburg, February 1861), in Kulishivka orthography with ъ and
  printed stress accents, heavy bold bilevel Google Books scan, the 600 ppi JBIG2 page image extracted unchanged from
  the Commons scan [2. Основа (Лютий
  1861).pdf](https://commons.wikimedia.org/wiki/File:2._%D0%9E%D1%81%D0%BD%D0%BE%D0%B2%D0%B0_%28%D0%9B%D1%8E%D1%82%D0%B8%D0%B9_1861%29.pdf)
  (PD-1923, PD-RusEmpire) as PNG, without the separate “Digitized by Google” overlay image. Truth from the validated
  Ukrainian Wikisource page `Сторінка:2. Основа (Лютий 1861).pdf/237` (revision 264392, CC-BY-SA-4.0), with the word
  hyphenated across the foot of the page ending in *лу́-*, and four corrections against the print: *перека́зано; чи*
  (Wikisource has a comma), *невпо́кійну* (the accent on о is printed, missing on Wikisource), *Тимъ-то* (Wikisource
  adds an accent that is not printed) and *тіі ха́лепи* (no accent printed on тіі).
- `greek-anatoliko-zitima-1876.png`: page 14 of D. Ainian (d. 1881), *Περὶ τοῦ Ἀνατολικοῦ ζητήματος* (1876, no place
  printed), polytonic Greek with a worn, speckled print, rendered locally from the DjVu at its native 400 dpi
  (2019×3099, PNG, recompressed losslessly with oxipng) from the Commons scan [Περί του Ανατολικού ζητήματος Δημήτριος
  Αινιάν.djvu](https://commons.wikimedia.org/wiki/File:%CE%A0%CE%B5%CF%81%CE%AF_%CF%84%CE%BF%CF%85_%CE%91%CE%BD%CE%B1%CF%84%CE%BF%CE%BB%CE%B9%CE%BA%CE%BF%CF%8D_%CE%B6%CE%B7%CF%84%CE%AE%CE%BC%CE%B1%CF%84%CE%BF%CF%82_%CE%94%CE%B7%CE%BC%CE%AE%CF%84%CF%81%CE%B9%CE%BF%CF%82_%CE%91%CE%B9%CE%BD%CE%B9%CE%AC%CE%BD.djvu)
  (PD-scan/PD-old-70; the author died in 1881). Truth from the validated Greek Wikisource page `Σελίδα:Περί του
  Ανατολικού ζητήματος Δημήτριος Αινιάν.djvu/14` (revision 106764, CC-BY-SA-4.0), keeping the printed *οἵκτου* that
  Wikisource marks with {{sic}}, and three corrections to what is printed: the misprints *Καρβοὺρ* and *χρσσιμεύσωσι*
  (Wikisource silently corrects them to Καβοὺρ and χρησιμεύσωσι) and the omitted article in *μεταξὺ τῶν Ἑλλήνων*.
- `bengali-anandamath-1938.png`: page 11 (book 1, chapter 3) of Bankim Chandra Chattopadhyay (d. 1894), *আনন্দমঠ*
  (Calcutta: Bangiya Sahitya Parishad, Ashadh 1345 = 1938), in old Bengali orthography with a smudged final line,
  rendered locally from the DjVu at its native resolution (1847×2630, PNG) from the Commons scan [আনন্দমঠ - বঙ্কিমচন্দ্র
  চট্টোপাধ্যায়.djvu](https://commons.wikimedia.org/wiki/File:%E0%A6%86%E0%A6%A8%E0%A6%A8%E0%A7%8D%E0%A6%A6%E0%A6%AE%E0%A6%A0_-_%E0%A6%AC%E0%A6%99%E0%A7%8D%E0%A6%95%E0%A6%BF%E0%A6%AE%E0%A6%9A%E0%A6%A8%E0%A7%8D%E0%A6%A6%E0%A7%8D%E0%A6%B0_%E0%A6%9A%E0%A6%9F%E0%A7%8D%E0%A6%9F%E0%A7%8B%E0%A6%AA%E0%A6%BE%E0%A6%A7%E0%A7%8D%E0%A6%AF%E0%A6%BE%E0%A6%AF%E0%A6%BC.djvu)
  (PD-scan/PD-India-URAA). Truth from the validated Bengali Wikisource page `পাতা:আনন্দমঠ - বঙ্কিমচন্দ্র
  চট্টোপাধ্যায়.djvu/৩৩` (revision 1651055, CC-BY-SA-4.0), corrected to the print in thirteen places where Wikisource
  modernised or dropped text: *এক জন* (six times, Wikisource *একজন*), *খল খল*, the old reph doublings *মূর্ত্তিসকল*,
  *পার্শ্ববর্ত্তী*, *খর্জ্জুর*, *প্রেতমূর্ত্তি*, the missing words in *কল্যাণী, কন্যা লইয়া শুইয়া ছিল* and the danda
  before the closing quote in *খাই।”*.
- `tamil-madras-aquarium-1921.jpg`: page 11 of James Hornell (d. 1949), *சென்னை மீன் செய்குளங்களின் அறிவிப்பு புஸ்தகம்*
  (guide to the Madras aquarium; Madras: Government Press, 1921), in pre-reform Tamil letterforms on yellowed paper with
  Latin-script glosses, the JPEG page image extracted unchanged from the Commons scan [1921-சென்னை மீன் செய்குளங்களின்
  அறிவிப்பு
  புஸ்தகம்.pdf](https://commons.wikimedia.org/wiki/File:1921-%E0%AE%9A%E0%AF%86%E0%AE%A9%E0%AF%8D%E0%AE%A9%E0%AF%88_%E0%AE%AE%E0%AF%80%E0%AE%A9%E0%AF%8D_%E0%AE%9A%E0%AF%86%E0%AE%AF%E0%AF%8D%E0%AE%95%E0%AF%81%E0%AE%B3%E0%AE%99%E0%AF%8D%E0%AE%95%E0%AE%B3%E0%AE%BF%E0%AE%A9%E0%AF%8D_%E0%AE%85%E0%AE%B1%E0%AE%BF%E0%AE%B5%E0%AE%BF%E0%AE%AA%E0%AF%8D%E0%AE%AA%E0%AF%81_%E0%AE%AA%E0%AF%81%E0%AE%B8%E0%AF%8D%E0%AE%A4%E0%AE%95%E0%AE%AE%E0%AF%8D.pdf)
  (tagged pd-old-100; strictly the author died in 1949, so PD-old-70, and a 1921 Indian government publication). Truth
  from the validated Tamil Wikisource page `பக்கம்:1921-சென்னை மீன் செய்குளங்களின் அறிவிப்பு புஸ்தகம்.pdf/13` (revision
  1509132, CC-BY-SA-4.0), with one correction to the print (*பெரிய விலாங்குகளுக்குப்*, Wikisource *விலங்குகளுக்குப்*);
  word spacing follows Wikisource, the tight justified print leaves several word gaps ambiguous.
- `hebrew-bostenay-1872.png`: page 42 of M. Lehmann (d. 1890), *בוסתנאי*, translated by S. J. Fuenn (ש. י. פ., d. 1890)
  (Vilnius, 1872), unpointed Hebrew in a small, bleeding bilevel print, the 300 ppi JBIG2 page image extracted unchanged
  from the Commons scan [Bostenay - Meir
  Leheman.pdf](https://commons.wikimedia.org/wiki/File:Bostenay_-_Meir_Leheman.pdf) (PD-scan/PD-old-auto-expired) as
  PNG. Truth from the validated Hebrew Wikisource page `עמוד:Bostenay - Meir Leheman.pdf/46` (revision 823311,
  CC-BY-SA-4.0), with the bidi control characters removed, the em dashes as printed (Wikisource has en dashes), the
  quotation marks reduced to those actually printed (Wikisource adds them around every speech) and written as ASCII "
  because the printed shapes («, „, “) are not reliably distinguishable at this resolution, and corrections to the
  print: *מלאו* (Wikisource מלאי), *המחשבות* (Wikisource מחשבות), *אתו* in *לשמור אתו* (Wikisource אותו), the comma in
  *נקמת אמו, אם*, *ענה המלך, העלם* and no colon after *וישאל*.
- `chinese-luxun-fengzheng-1948.png`: page 487 of Lu Xun (d. 1936), *野草*: 「風箏」 in *魯迅全集* vol. 1 (Shanghai: 魯迅全集出版社,
  1948), vertical traditional Chinese with a ruled running head, the 1-bit page mask (3500×4500) extracted unchanged
  from the Commons scan [魯迅全集01
  (1948).pdf](https://commons.wikimedia.org/wiki/File:%E9%AD%AF%E8%BF%85%E5%85%A8%E9%9B%8601_%281948%29.pdf) (PD-old)
  and saved as PNG with its polarity inverted, which is how the PDF paints it (black text on white). Truth from the
  validated Chinese Wikisource page `Page:魯迅全集01 (1948).pdf/508` (revision 5977126, CC-BY-SA-4.0), keeping the printed
  variants 偸, 卽, 祕, 終于; no corrections.
- `czech-certuv-svagr-1899.png`: page 4 of Božena Němcová (d. 1862), *Čertův švagr* in *Národní báchorky* (Praha: I. L.
  Kober, 1899), Czech antiqua on toned paper with guillemet-style quotation marks »…«, rendered locally from the DjVu at
  its native 300 dpi (1577×2425, PNG, recompressed losslessly with oxipng) from the Commons scan [Božena Němcová -
  Čertův švagr — Kmotr Matěj -
  1899.djvu](https://commons.wikimedia.org/wiki/File:Bo%C5%BEena_N%C4%9Bmcov%C3%A1_-_%C4%8Cert%C5%AFv_%C5%A1vagr_%E2%80%94_Kmotr_Mat%C4%9Bj_-_1899.djvu)
  (PD-old). Truth from the validated Czech Wikisource page `Stránka:Božena Němcová - Čertův švagr — Kmotr Matěj -
  1899.djvu/6` (revision 306162, CC-BY-SA-4.0); no corrections.
- `english-macaulay-minutes-1862.pdf`: pages 111–112 (PDF pages 113–114) of T. B. Macaulay (d. 1859), *Macaulay's
  Minutes on Education in India*, ed. H. Woodrow (Calcutta: C. B. Lewis, Baptist Mission Press, 1862), a two-page PDF
  cut with `qpdf --empty --pages … 113-114` from the Internet Archive scan [Macaula yʼs minutes on education in India,
  written in the years 1835, 1836 and 1837 (IA
  dli.csl.7615).pdf](https://commons.wikimedia.org/wiki/File:Macaula_y%CA%BCs_minutes_on_education_in_India,_written_in_the_years_1835,_1836_and_1837_%28IA_dli.csl.7615%29.pdf)
  (PD-old-100-expired, IA dli.csl.7615), keeping the original 300 ppi JPEG page images and the hidden ABBYY FineReader
  12 text layer unchanged; the layer is good but imperfect (*o f*, *W e*, *H e*, *th q ft* for *they*, *Vf e* for *We*,
  *lind* for *find*, *far* for *for*, *wo* for *we*, *bo* for *be*, stray *^*, soft hyphens kept, garbage from the
  library stamp and CSL logo). Truth from the validated English Wikisource pages `Page:Macaula yʼs minutes on education
  in India, written in the years 1835, 1836 and 1837 (IA dli.csl.7615).pdf/113` (revision 16332395) and `/114` (revision
  16330991) (CC-BY-SA-4.0), joined into one text with the word *por-* hyphenated across the foot of page 112 and
  corrected to the print in ten places where the Wikisource text follows another edition: *these phenomena*,
  *testimonials,’ they say*, *means,’ they say*, *Sanscrit colleges*, *a nest, not merely of helpless place-hunters, but
  of bigots*, *From the native society left to itself, we have no difficulties to apprehend; all the murmuring*, *called
  into being, and nursed*, *another fact, which*, *The Committee* (twice) with *a lac of rupees*, and *the libraries, or
  rather the lumber-rooms, of this body*.

## Real born-digital documents

These PDFs are unchanged as published, each with a truth derived from the publisher's own XML or HTML and checked
against the rendered pages and `pdftotext`.

Conventions for all truths: UTF-8 NFC, one paragraph per line, blank line between paragraphs; table rows one per line
(cells joined by a single space, no blank lines between rows); references one per line as printed (blank line between).
Text comes from the publisher's XML/HTML; layout-only details (heading numbers, run-in headings, author/affiliation
lines, quote glyphs, reference wording) follow the printed page where the two differ. Superscript marks (affiliations,
footnotes, citation numbers) are attached directly to the preceding text (`Montenegroa,∗`, `habitantes1`). A leading
footnote/affiliation label is followed by a space (`a Programa…`, `† These…`). Line-break hyphens are joined, real
hyphens kept. A paragraph broken by a column/page break, float or page-bottom note is kept whole on one line, and the
interrupting block (footnotes, page footer, table, figure caption, sidebar box) follows that paragraph. Always left out:
running heads/footers, page numbers, logos, Crossmark/“Check for updates” badges, and text inside figures (axis labels,
tick values, legends). PMC files were taken from the PMC Open Access cloud bucket (`pmc-oa-opendata`); the MD5s match
the PMC metadata JSON.

- `article-biollett-2016.pdf`: P. Morán, L. Labbé, C. Garcia de Leaniz, “The male handicap: male-biased mortality
  explains skewed sex ratios in brown trout embryos”, *Biology Letters* 12: 20160693 (2016), Royal Society, DOI
  [10.1098/rsbl.2016.0693](https://doi.org/10.1098/rsbl.2016.0693), PMC5206587. Licence: [CC BY
  4.0](https://creativecommons.org/licenses/by/4.0/), stated on page 1. PDF:
  `https://pmc-oa-opendata.s3.amazonaws.com/PMC5206587.1/PMC5206587.1.pdf`; XML (JATS):
  `https://pmc-oa-opendata.s3.amazonaws.com/PMC5206587.1/PMC5206587.1.xml`. All 4 pages, unchanged. Producer: Acrobat
  Distiller 8.1.0 (Windows); Creator: Arbortext Advanced Print Publisher 9.1.520/W. Layout: page 1 has a left sidebar
  plus a wide main column, pages 2–3 have two columns, and the references are in three columns. Truth: the page-1
  sidebar comes first (top to bottom), then the main column. Body, abstract, captions and Table 1 come from the XML.
  Added from the page image: sidebar items (`rsbl.royalsocietypublishing.org`, “Research”, “Cite this article…”,
  Received/Accepted dates, Subject Areas, Keywords in printed comma form, “Author for correspondence”), the
  “Evolutionary biology” banner, the ORCID line (`PM, 0000-…; CGL, 0000-…`), the heading numbers “1.”–“4.”, run-in
  back-matter headings (“Ethics.”, “Data accessibility.” …, in printed order with Acknowledgements last), the copyright
  box, and all 29 references re-keyed as printed (the XML citations are structured). The straight apostrophe in the
  XML's “Fisher's Exact test” was changed to the printed ’. Left out: the BIOLOGY LETTERS and Royal Society logos,
  open-access/Crossmark icons, the vertical running heads “rsbl.royalsocietypublishing.org” / “Biol. Lett. 12:
  20160693”, page numbers 2–4, and the text inside Figures 1–2 (raster JPEGs). Note: the PDF fonts map =, ±, <, −, ° to
  ¼, +, ",", 2, 8, so pdftotext shows e.g. `SR ¼`, `p , 0.001`, `20.30`, `12.18C`. The truth has the true characters
  (`SR =`, `p < 0.001`, `−0.30`, `12.1°C`).
- `article-bmcresnotes-2020.pdf`: S. Ahmady, N. Khajeali, M. Kalantarion, M. Amini, “A qualitative content analysis of
  "problem students": how can we identify and manage them?”, *BMC Research Notes* 13:566 (2020), BioMed Central /
  Springer Nature, DOI [10.1186/s13104-020-05393-8](https://doi.org/10.1186/s13104-020-05393-8), PMC7745484. Licence:
  [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/), stated on page 1. PDF:
  `https://pmc-oa-opendata.s3.amazonaws.com/PMC7745484.1/PMC7745484.1.pdf`; XML:
  `https://pmc-oa-opendata.s3.amazonaws.com/PMC7745484.1/PMC7745484.1.xml`. All 5 pages, unchanged. Producer: Acrobat
  Distiller 10.1.8 (Windows); modified using iText 5.3.5 (SPRINGER SBM); Creator: Springer. Layout: two columns, a
  full-width abstract box, and a full-width Table 1 at the bottom of page 3. Body, list, Table 1 and back-matter notes
  come from the XML. Added from the page image: the page-1 masthead (DOI line, “BMC Research Notes”, “RESEARCH NOTE”,
  “Open Access”), the printed author line, the correspondence footnote, the licence box text (printed with zero-width
  spaces inside the URLs; the truth uses plain URLs), the “– ” list dashes, the “Author details” paragraph (the XML
  affiliations carry unprinted GRID ids), “Received/Accepted”, “Published online: 17 December 2020”, all 15 references
  as printed, the Publisher’s Note, and the BMC “Ready to submit your research?” advert box (printed text, kept).
  Straight apostrophes in the XML were changed to ’ where the PDF has ’. Quote-glyph counts now match the PDF exactly,
  including the mixed straight/curly quotes the PDF really has. Doubt: “Published online: 17 December 2020” is a 1-bit
  image (600 ppi), not text, so text-layer extractors cannot get it. It is kept because a reader sees it. Headings like
  “Self‑regulation skills” use U+2011 in the PDF; the truth uses “-”. Left out: running head “Ahmady et al. BMC Res
  Notes (2020) 13:566”, “Page n of 5”, BMC logos, the Check-for-updates badge.
- `article-atenprimaria-2026.pdf` (Spanish): C. C. Torres Montenegro, H. Torres Pinzón, J. S. Therán León, L. E.
  Echeverría Correa, “Dinámica familiar y extrafamiliar en adultos mayores con baja adherencia al tratamiento
  farmacológico en un programa de falla cardiaca en Santander, Colombia” (Cartas científicas), *Atención Primaria* 58
  (2026) 103552, Elsevier España, DOI [10.1016/j.aprim.2026.103552](https://doi.org/10.1016/j.aprim.2026.103552),
  PMC13284469. Licence: [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/), stated on page 1. PDF:
  `https://pmc-oa-opendata.s3.amazonaws.com/PMC13284469.1/PMC13284469.1.pdf`; XML:
  `https://pmc-oa-opendata.s3.amazonaws.com/PMC13284469.1/PMC13284469.1.xml`. All 3 pages, unchanged. Producer: Acrobat
  Distiller 26.0 (Windows); Creator: Elsevier. Layout: two columns, with a full-width Table 1 (three sub-sections) on
  page 2. Body, table and table notes come from the XML. Added from the page image: the masthead (“Atención Primaria”,
  “www.elsevier.es/ap”, “CARTAS CIENTÍFICAS”), the author line in printed form (“… y Luis Eduardo…”), affiliations a–d,
  the correspondence footnote, the page-1 DOI/copyright footer, and the 6 references as printed (structured in the XML).
  Left out: running heads “Atencion Primaria 58 (2026) 103552” (pages 1 and 3) and the page-2 author running head, page
  numbers, the ELSEVIER logo and cover thumbnail, the Check-for-updates badge. Note: the PDF maps the em dash to “----”
  and the en dash to “---” in pdftotext output. The truth uses — and – as in the XML and on the page. The
  corresponding-author mark is written ∗ (U+2217, as the PDF maps it; the XML has ⁎). Doubt: the article's own text
  openly says Claude (Anthropic) was used for language editing, and the ethics section still contains the authors'
  unfilled placeholders “[a completar …]”. Both are printed and kept verbatim.
- `cdc-mmwr-prep-2024.pdf`: M. E. Peck, S. Davis, E. Odoyo-June, et al. (36 authors), “Progress Toward UNAIDS Global HIV
  Pre-Exposure Prophylaxis Targets: CDC-Supported Oral Pre-Exposure Prophylaxis — 37 Countries, 2017–2023”, *MMWR Morb
  Mortal Wkly Rep* 2024;73(47):1082–1086, CDC, DOI [10.15585/mmwr.mm7347a3](https://doi.org/10.15585/mmwr.mm7347a3). US
  federal government work (public domain, 17 U.S.C. §105). cdc.gov answers this machine with 403, so both files were
  fetched as raw Wayback captures (`id_`). PDF: `https://www.cdc.gov/mmwr/volumes/73/wr/pdfs/mm7347a3-H.pdf`, capture
  `https://web.archive.org/web/20241127215343id_/…` (SHA-1 matches the CDX digest 3KYCOVQ35C4CCURBWQS4XEA6ADLAG3HC,
  identical in every capture through 2026-07). HTML: `https://www.cdc.gov/mmwr/volumes/73/wr/mm7347a3.htm`, capture
  20241204092203. All 5 pages (article PDF pp. 1082–1086), unchanged. Producer: Adobe PDF Library 17.0; Creator: Adobe
  InDesign 20.0 (Windows). Layout: two columns, Table 1 in the right column of page 2, a full-width Table 2 on page 3,
  and a shaded Summary box at the top of page 4. Body, Summary box, tables, table notes, footnotes, acknowledgment and
  affiliations come from the HTML. Changes to match the printed page: the HTML's “2017─2023” (box-drawing U+2500, an
  HTML conversion artefact) is written “2017–2023” as printed. The author line follows print, which drops some commas
  (“Megan E. Peck PhD1”, “Carlos Castaneda MD22”, “Gaston Djomand MD1; Carlos Toledo PhD1; Ramona Bhatia MD1”). The
  HTML's run-in table-note paragraphs are split into one note per line as printed. Table 2's three header rows follow
  the printed grouping (“All PrEP initiations, no. (%)” / “Sex Age group, yrs Key population initiations,§ no. (%)” /
  column labels), not the HTML's rowspan structure. In references 7–8 PMID comes before the DOI as printed (the HTML has
  them reversed), and the U+200E marks in reference 2 were removed. Reference numbers “1.”–“9.” are added as printed.
  Data typos printed in both sources are kept (“79,770 (35.0)”, “12,8836 (5.7)”, “Philippines Thailand”). Left out:
  running head “Morbidity and Mortality Weekly Report”, page numbers 1082–1086 and the footer “U.S. Department of Health
  and Human Services | Centers for Disease Control and Prevention | MMWR | November 28, 2024 | Vol. 73 | No. 47”.
  HTML-only material was not used (dateline, suggested citation, web chrome, “Top” links).

## Generated documents

`truth/slides.txt`, `inventory.txt`, `letters.txt`, `events.txt` and `contacts.txt` are text written for this corpus,
under the repository's licence, like `prose.txt` and `report.txt`. `truth/mixed.txt` puts together paragraphs of the
corpus's own translations of the lighthouse story (`prose`, `arabic`, `chinese`, `hindi`, `hebrew`, `russian`,
`greek`). `generate.sh` renders them with real producers: LibreOffice (presentations, spreadsheets, the
mixed-language documents and, through its PDF import, the SVG), DuckDB (Parquet), pandoc (FictionBook), calibre
(MOBI), LaTeX and dvips (DVI, PostScript), AbiWord, Scribus, mkvmerge (a Matroska file holding the subtitles),
xorriso (ISO 9660), gcab (cabinet) and rpmbuild (RPM). The e-mail, iCalendar, vCard, SubRip and DXF files are
written by `mail.py` (Python's `email` and `mailbox`), `ical.py`, `vcard.py`, `srt.py` and `dxf.py` (AutoCAD R12
`TEXT` entities, one to a wrapped line).
