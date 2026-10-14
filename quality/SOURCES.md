# Sources of the ground truth

The texts in `truth/` named after a language that the rest of the corpus does not use (`albanian.txt` to `ukrainian.txt`, other
than `danish`, `swedish`, `norwegian`, `german`, `polish`, `czech`, `turkish`, `greek`, `russian` and the scripts of
the `script` category) are the title, the preamble and the first articles, about 200 words, of each language's
translation of the Universal Declaration of Human Rights, as collected by the
[UDHR in Unicode](https://github.com/eric-muller/udhr) project (`data/udhr/udhr_<code>.xml`). Spaces before commas
and full stops are removed and U+2010 hyphens are written as `-`; the text is otherwise unchanged. Serbian is in
Cyrillic (`serbian`) and Latin (`serbian-latin`), Norwegian in Bokmål (`norwegian`) and Nynorsk (`nynorsk`).
