# How dijo goes beyond Tika

What dijo does differently from Apache Tika, grouped by its three goals. Each item gives the mechanism and its
evidence: a figure from `quality/`, `conformance/`, the README or the CHANGELOG, or the commit or issue that landed it.
`quality/COMPARISON.md` scores dijo, Docling 2.131.0, Kreuzberg 4.10.4 and Tika 4.1.0 on 240 ground-truth documents.
`quality/SCOREBOARD.md` holds dijo's current scores, and `just quality` fails when a score falls below
`quality/baseline.json`.

## Accuracy

- **Overall.** Over the 240 compared documents dijo recalls 99.6% of words and Tika 84.8%. Mean CER is 0.004
  against 0.160, and mean spurious text 0.003 against 0.125 (`quality/COMPARISON.md`).
- **PDF text layers are judged page by page.** A port of `TextLayerQuality` scores unmapped glyphs, impossible
  characters, mixed alphabets and vowel-less words (`dijo-pdf/src/formats/pdf/quality.rs`). OCR replaces a garbage
  layer, and a doubtful one is read both ways, keeping the better reading. CER on `prose-broken-private-use.pdf` is
  0.000 against Tika's 0.987, and on `prose-broken-offset.pdf` 0.000 against 0.829.
- **Reading order.** PDF lines are ordered with the CTM applied, not the text matrix alone (#212).
  `prose-columns-reversed.pdf` has CER 0.000 against Tika's 0.745.
- **Normalisation for indexing.** Hyphenated words are joined across lines, ligatures expanded, text put in NFC,
  invisible characters dropped. `hyphenated.pdf` has a token error rate of 0.006 against Tika's 0.290. Running
  headers and footers are learnt across every window of pages, left and right pages apart (#201, #213).
- **Complex scripts in PDFs.** CER on `thai.pdf` is 0.007 (Tika 0.958), `hindi.pdf` 0.000 (Tika 0.838) and
  `arabic.pdf` 0.178 (Tika 0.835).
- **Legacy encodings.** On `japanese-shift_jis.txt` dijo's CER is 0.000 and Tika's 1.008. Of the 52
  legacy-encoding rows, all score 0.000 except `romanian-iso-8859-16.txt` at 0.032 (`SCOREBOARD.md`).
- **Language detection.** dijo starts from the logits of Tika 4's CharSoup and adds two things: a -2 prior for 41
  regional and constructed languages, and a logistic regression over hashed words and character 1- to 5-grams for
  each group of close national languages (db5fe871). On single sentences it is right 86.7% of the time, against
  CharSoup's 80.5% (README). On Tatoeba the macro average rises from 0.788 to 0.849, and no language gets worse.
- **OCR picks its model per page.** Every release carries a tesseract model for each national language of Europe and
  for each other script detection finds. Each page is read again with the model of the language detected on it
  (0.2.4).
- **OCR preprocessing.**
  - Deskew and orientation: `prose-scan-skewed.png` CER 0.000, Tika 0.950.
  - Division by the background under uneven light: `prose-scan-shadow.png` 0.002, Tika 0.312 (#227).
  - Perspective flattening of photographed pages (`flatten.rs`).
  - Light-on-dark regions inverted by dijo itself, so tesseract's inverted retry can be switched off (#331).
  - Small text beside photos enlarged (#306).
  - The image resolution passed to tesseract, so columns stay apart (#209).
- **Old print.**
  - Blackletter: a page that shows the long-s signature is read again with `deu_latf`. Two 1895 Fraktur pages fall
    from CER 0.118 to 0.057 and from 0.213 to 0.162 (#225).
  - The long s that tesseract reads as f is restored from the page's language dictionary (#210).
- **Scripts.** CJK and Thai scans are read without a space between every character (#205), and Hebrew is no longer
  taken for Thai (#204).
- **Noise is dropped.** A low-confidence reading that is mostly digits and marks, or in no language, is dropped and
  flagged `"noise":true` (#194).
- **Image formats.** On the quality corpus Tika returns no text for the WebP, HEIC, AVIF, PBM, DjVu, JB2, PSD, PCX and
  TGA scans, and dijo reads each of them with CER 0.000 (`COMPARISON.md`).
- **Tika and JVM quirks are fixed on purpose.**
  - Output follows document order, not Java hash-map order.
  - Malformed text decodes as WHATWG specifies (0.2.2).
  - No result depends on the machine's time zone, and numbers do not wrap like Java's `int` and `long` (#193).

## Ideas from PaddleOCR research

dijo takes models and techniques from PaddleOCR / PP-OCRv5 (PaddlePaddle, Apache-2.0; commit pinned in
`ci/ppocr.env`) and runs them in-process (`dijo-image/src/ocr/ppocr.rs`, `recognize.rs`).

- **Second-pass cascade, gated by confidence.** Tesseract reads every page first. A page read at mean word
  confidence 90 or less (`MAX_REREAD_CONFIDENCE`) is read again with PP-OCRv5, and the more confident reading is kept.
  This applies to Latin, Devanagari, Chinese and Japanese pages that are not blackletter. Confident pages cost
  nothing extra (5af5d649, #207).
  - CER effect (README): *Epigrammata* 1520 from 16.0% to 4.5%, *Quixote* 1605 from 12.0% to 5.8%, *Zoologia
    Danica* 1779 from 10.3% to 5.7%, and two 1895 Devanagari pages from 33.1% to 14.6% and from 52.8% to 21.4%.
    The current scoreboard has the second Devanagari page at 0.234.
  - Raising the gate to 95 made clean modern scans worse, so it stays at 90.
- **DBNet text detection** (PP-OCRv5_mobile_det).
  - Input is scaled to a long side of 960 on a stride of 32 and normalised with ImageNet's mean and deviation.
  - PaddleOCR's post-processing is kept: pixels are binarised at 0.3, a region is kept when its own pixels average
    at least 0.6, and the box is unclipped by a ratio of 1.5 using the region's length and thickness.
  - Scoring a region by its own pixels rather than its bounding box keeps slanted lines (5bc7432d): *Quixote* went
    from 7.2% to 5.8% and *Zoologia* from 9.9% to 5.7%.
- **One CTC recogniser per script.** The Latin and Devanagari PP-OCRv5 mobile recognisers, plus PP-OCRv5_mobile_rec
  for Chinese and Japanese.
  - Each line crop is scaled to 48 px high at its own width and padded to 320 px, as PaddleOCR pads it.
  - Output is decoded by greedy CTC against the labels in the model's `inference.yml`.
  - Lines below 0.5 confidence are dropped. The page's confidence is the character-weighted mean over every line,
    dropped ones included, so a reading cannot win by discarding most of a page (5bc7432d).
  - The Cyrillic recogniser lacks pre-reform letters and tripled the errors on a 1901 Russian page. The Arabic, Greek
    and Thai recognisers improved no row. None of the four is bundled (5af5d649).
- **Lines follow tesseract's layout.** PP-OCR lines are grouped by the blocks tesseract found, then by rows. A line
  that lies in no block is placed where it falls in its column (#358).
- **Vertical CJK.** Detected boxes at least 1.5 times taller than wide, PaddleOCR's own threshold, are merged into
  columns ordered right to left. Each column is turned a quarter, as PaddleOCR turns it, and read with the CJK
  recogniser. CER on `japanese-ndl-1910s` fell from 1.000 to 0.233 and on `japanese-ndl-1920s` from 1.000 to
  0.136. Tesseract's `jpn_vert` was measured and read worse (#322, bbb4ace7).
- **UVDoc page dewarping** (PaddlePaddle/UVDoc).
  - Runs only when the page is curled (at least 1°) and was read at confidence 90 or less.
  - Tesseract reads the flattened page again, and that reading is kept only if it is at least 10 points more
    confident.
  - On `prose-photo-curled.jpg` CER fell from 57.9% to 2.4%, with every other row unchanged. It takes about 0.3 s
    per 150 dpi page (36dfcf9f, 7a06904f, #228).
- **Reproducible ONNX conversion.**
  - `ci/convert-ppocr.sh` converts PaddlePaddle's official weights with paddle2onnx 2.1.0 at opset 14. UVDoc uses
    opset 16, because its GridSample needs it.
  - The conversion reproduces the models bit for bit, and `ci/ppocr.tsv` pins each model by SHA-256.
  - Ready-made conversions were rejected. RapidOCR's Latin model has 502 of the 836 characters and no long s; others
    use a BatchNormalization attribute rten refuses (5af5d649).
- **In-process pure-Rust runtime.** The models run on rten (now 0.28, f922a5a8), so no native library or Python
  joins the build. Each model is loaded lazily, once per process (`OnceLock`), and shared by every thread.
- **Activations count against the memory budget.** Detection activations are reserved at 128 bytes per input pixel
  (115 measured) and UVDoc's at 48 (31–43 measured), so an oversized page fails cleanly with `MemoryLimit`.

## Throughput and memory

- **Benchmarks** (`hyperfine`, README):

  | Input | dijo | `tika-app` 4.1.0 |
  | --- | --- | --- |
  | 28-page PDF | 55 ms, 23 MiB | 4.0 s, 301 MiB |
  | DOCX | 6 ms, 17 MiB | 3.3 s, 213 MiB |

- **A static, in-process binary.** Tika, GraalVM and JNI were removed in 0.2.2. The static Linux binaries need no C
  library and run `FROM scratch`. Tesseract, the language data, the PP-OCR models and the HEIC/AVIF decoder are
  linked or embedded. No `tesseract` on `PATH` or `heif-dec` is ever run (#317).
- **Bounded memory on large inputs.**
  - Files over 64 MiB are mapped, and pages already read are released with `MADV_DONTNEED` as parsing goes on
    (`dijo-base/src/input.rs`).
  - Parsers declare a bounded footprint (#179), and `just test-large` checks peak memory on generated inputs up to
    1 GiB.
  - A 512 MiB DOCX peaks at 78 MiB (0.2.7).
- **A memory budget per extraction.** A counting global allocator charges every allocation to its extraction, and
  memory used by C libraries (tesseract, rten) is reserved explicitly. `--max-heap-bytes` fails one extraction
  cleanly instead of the process (`dijo-base/src/memory.rs`).
- **Allocator tuning.** glibc arenas are capped at the processor count, and blocks of 4 MiB or more get their own
  mapping, so a long-running server's memory stays level (0.2.7, 0.2.11).
- **Hot paths.** Some 75 targeted changes (#230–#304) remove per-cell, per-glyph and per-pixel allocations and
  repeated lookups, with output unchanged.
- **Fewer OCR passes.**
  - A page of a known language is read once instead of four or five times (#198), and an image is enlarged only when
    its lines are small (#199): old print takes a third less CPU.
  - The first image's language is detected from a strip of six lines, about a fifth less CPU on Latin scans (#218).
  - `invert_threshold=0` cuts 3.4% of OCR CPU on the corpus, and up to 27% on vertical Japanese (#331).
- **Concurrency.** OCR'd PDF pages are rendered and read in parallel while output streams in order. CLI batches run
  one job per core in worker processes. `--isolation process` confines a crash or runaway document to one request.
- **Tesseract patches** (`ci/tesseract-patches/`, rationale in `ci/tesseract.nix`):

  | Patch or build change | Effect |
  | --- | --- |
  | `mmap-models` | Models are mapped read-only and their dictionaries (88.8 of `script/Latin`'s 89.4 MB) read in place, shared by every process through the page cache. Peak for a Latin page falls from 305 MB to 191 MB, about 85 MB less per worker (#211) |
  | `adapts-only-while-learning`, `lstm-skips-universal-ambigs` | A chi_sim engine takes 9 MiB instead of 43, eng 2 instead of 10, OSD 19 instead of 32 (#308, #311) |
  | `inline-stridemap` | 10–13% faster per page; `-O3` adds about 5% |
  | `lstm-copies-in-place`, Pix word images, skipped fixed-pitch test | Up to a quarter fewer instructions per page, same text (#217, #219, #223) |
  | `shares-page-layout` | Layout analysis was 28% of corpus reading time; a reread with another model now saves 400 ms (#334) |
  | `rows-restart-in-place` | A page of thousands of tiny words reads in seconds instead of hanging (#316) |
  | `takes-empty-pages-as-read` | An empty layout is not analysed again for every confidence query (#359) |
  | No OpenMP | One page per thread instead: an OpenMP team cost 2.6× the CPU for a 1.2× faster single page (#62) |

## File types

Tika reads none of these, or only their summary information or raw lines; dijo reads their content
(CHANGELOG, Unreleased):

- **Office and documents.** Lotus 1-2-3 (#347), Lotus Word Pro (#346), Lotus Notes `.nsf` (#356), AbiWord (#348),
  Scribus (#349), MS Project task plans (#341; Tika reads only the summary), MS Works word processor text (#342;
  Tika: summary only), Windows Help `.hlp` (#350), TeX DVI (#353), PostScript/EPS (#336).
- **Archives and images of disks.** ISO 9660 with Rock Ridge and Joliet (#338), Microsoft Cabinet including OneNote
  packages (#339), RPM (#340), Apple DMG with HFS+ (#355).
- **Data, media and other.** Apache Parquet (#352), Matroska/WebM subtitle tracks and tags (#337), Flash SWF (#357),
  BitTorrent metainfo (#354), AutoCAD DXF text (#351; binary DXF detected where Tika's magic misses it).
- **Structured reads where Tika dumps raw text.** iCalendar (#343), vCard (#344), SVG visible text only (#345).
- **Coverage of Tika's own formats.** Every format Tika reads is read natively; Pack200 was the last (0.2.3).

## Compatibility

- **tika-server 4.1 API.** Implemented in full: `/tika[/handler]`, `/rmeta`, `/meta`, `/unpack`, `/detect`,
  `/language`, `/version`, `/parsers`, `/detectors` and `/mime-types` (#320). A client switches by changing its base
  URL. The README lists every difference from tika-server.
- **tika-server 3.x clients.** The JSON forms of 3.x are served. Every 3.x metadata key is derived from Tika's own
  3.x-to-4.x key table and `LegacyKeyMigrationFilter`'s prefix rules, and the endpoints both versions share carry
  both keys (#363).
- **Conformance harness.** `just conformance` extracts Tika's 1094 test documents natively and with the same Tika on
  the JVM. 1051 match (token F1 of at least 0.95) or are empty on both sides (`conformance/MATRIX.md`). The README
  states that most of the rest are Tika bugs dijo fixes on purpose.

## Measured and rejected

- **Neural post-OCR correction (#228).** Rescoring PP-OCR's CTC alternatives with Qwen2.5-0.5B improved *Quixote* by
  9% only, left the Hindi pages unchanged or worse, made *Epigrammata* worse, and no setting helped every row. It
  would add 0.3–1 GB of RSS, a bundle 2–14× larger, and tens of seconds per page. No permissively licensed
  multilingual corrector exists.
- **Batched CTC recognition (#208).** Batching a page's lines into width buckets was slower on every runtime tried:
  the 35 lines of a *Quixote* page took 3.99 s batched against 2.55 s one at a time on onnxruntime, and 4.17 s
  against 2.29 s on tract. At these widths each call is already compute-bound. rten on exact widths matches
  onnxruntime.
- **Full-resolution DBNet detection.** Mean CER 3.83% against 3.80%, for about four times the detector's memory
  (5bc7432d).
- **More PP-OCR recognisers, and tesseract `jpn_vert`.** None improved any row (5af5d649, bbb4ace7).
