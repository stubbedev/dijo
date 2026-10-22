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
