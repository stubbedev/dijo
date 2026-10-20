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
