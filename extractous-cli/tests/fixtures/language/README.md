# Language detection fixtures

- `tatoeba.tsv`: language, Tatoeba sentence ID and sentence, about 200 per language for Danish, Swedish, Norwegian
  Bokmål and Nynorsk, Icelandic, Faroese, Finnish, Northern Sámi and their neighbours. The sentences are from
  [Tatoeba](https://tatoeba.org) (`https://tatoeba.org/sentences/show/<ID>`), licensed under
  [CC BY 2.0 FR](https://creativecommons.org/licenses/by/2.0/fr/); they are unchanged, and only a sample of each
  language's sentences is kept.
- `edge-cases.txt`: inputs written for this test, one per line with `\n`, `\t` and `\\` escaped, that exercise the
  preprocessing: URLs, e-mail addresses, diacritics and joiners that are skipped, scripts without spaces between
  words, words longer than the word buffer, and mixed scripts.
- `charsoup-reference.tsv`: what Apache Tika's own CharSoup detector answers for each input, written by
  `just charsoup-reference`. The port in `src/language/charsoup.rs` must answer the same.
