# How dijo goes beyond Tika

What dijo does differently from Apache Tika, grouped by its three goals. Each item gives the mechanism and its
evidence: a figure from `quality/`, `conformance/`, the README or the CHANGELOG, or the commit or issue that landed it.
`quality/COMPARISON.md` scores dijo, Docling 2.131.0, Kreuzberg 4.10.4 and Tika 4.1.0 (with tesseract 5.5.4 on its
path) on 333 ground-truth documents in 17 categories: 292 generated from texts in `quality/truth` by LibreOffice,
pandoc, LaTeX and the other producers `quality/generate.sh` runs, and 41 real ones listed in `quality/SOURCES.md`.
The real ones are scans with published transcriptions (newspapers from 1864 to 1994, books from 1520 to 1948 in
Latin, Cyrillic, Greek, Hebrew, Arabic, Armenian, Devanagari, Bengali, Tamil, Han and Japanese script, letters,
leases and financial tables), an Internet Archive PDF with its OCR layer, and two-column PDFs from three journals
and the CDC.
`quality/SCOREBOARD.md` holds dijo's current scores, and `just quality` fails when a score falls below
`quality/baseline.json`.

## Accuracy

- **Overall.** Over the 333 compared documents dijo recalls 96.4% of words and Tika 76.0%. Mean CER is 0.035
  against 0.252, and mean spurious text 0.029 against 0.197 (`quality/COMPARISON.md`). On the 240 documents compared
  before the corpus grew, recall is 99.7% against 84.7%; the 41 real documents and the new document classes are
  harder for every extractor.
- **Real scans.** Over the 122 OCR rows dijo's mean CER is 0.051 and recall 92.9%, Tika's 0.412 and 48.6%. Tika
  reads with English only, so it recalls nothing of the Greek, Bengali, Hebrew, Chinese and Ukrainian book pages,
  which dijo reads at CER 0.037, 0.083, 0.067, 0.168 and 0.192. A scanned financial table set in old-style figures
  (`unlv-table-9521-052.tif`) has CER 0.188 against Tika's 0.649, and Fraktur newspapers of 1864 and 1873 0.025 and
  0.092 against 0.156 and 0.265.
- **PDF text layers are judged page by page.** A port of `TextLayerQuality` scores unmapped glyphs, impossible
  characters, mixed alphabets and vowel-less words (`dijo-pdf/src/formats/pdf/quality.rs`). OCR replaces a garbage
  layer, and a doubtful one is read both ways, keeping the better reading. CER on `prose-broken-private-use.pdf` is
  0.000 against Tika's 0.987, and on `prose-broken-offset.pdf` 0.000 against 0.829.
- **Reading order.** PDF lines are ordered with the CTM applied, not the text matrix alone (#212).
  `prose-columns-reversed.pdf` has CER 0.000 against Tika's 0.745.
- **Normalisation for indexing.** Hyphenated words are joined across lines, ligatures expanded, text put in NFC,
  invisible characters dropped. `hyphenated.pdf` has a token error rate of 0.006 against Tika's 0.290. Running
  headers and footers are learnt across every window of pages, left and right pages apart (#201, #213).
- **Complex scripts in PDFs.** CER on `thai.pdf` is 0.007 (Tika 0.958), `hindi.pdf` 0.000 (Tika 0.832) and
  `arabic.pdf` 0.178 (Tika 0.838).
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
  - Division by the background under uneven light: `prose-scan-shadow.png` 0.000, Tika 0.312 (#227).
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
- **Where dijo is behind** (`quality/COMPARISON.md`; none of these is tuned yet):
  - `mixed.pdf`, one page with paragraphs in seven scripts, recalls 27.7% of words, against Docling's 83.5% and
    Kreuzberg's 88.1% (Tika 26.6%); the same page scanned recalls 28.8%. The ODT and DOCX of the same text score
    0.000.
  - `kronen-zeitung-1933.jpg`, a colour sports page mixing Fraktur, roman type and results tables, has CER 0.834
    against Kreuzberg's 0.300 and Tika's 0.487.
  - On the four real two-column PDFs dijo recalls 92.4% to 100% of words, but its CER (0.089 to 0.468) is higher
    than Docling's on three of them: `article-biollett-2016.pdf` 0.226 against 0.068,
    `article-atenprimaria-2026.pdf` 0.468 against 0.195, `cdc-mmwr-prep-2024.pdf` 0.314 against 0.228.
  - The PostScript that dvips writes (`prose.ps`) and the Scribus 1.7 document (`prose.sla`) yield no text.
  - Some scripts stay weak: the Ukrainian page of 1861 with printed stress accents recalls 24.6% of words, the
    Tamil page of 1921 58.3%.
  - Kreuzberg's CER is lower on `table.xlsx` (0.044 against 0.080) and `inventory.ods` (0.012 against 0.058), and
    Docling, Kreuzberg and Tika keep the subject lines of the letters in `letters.eml`, which dijo leaves out
    (recall 95.4% against 96.9%).
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
  joins the build. How they are loaded and charged to memory is under "Streaming and memory".
- **Under evaluation** (from PaddleOCR's papers list; each must hold accuracy, time per page and RSS before it
  ships): the Korean PP-OCRv5 recogniser (#372), PP-OCRv6 small/tiny recognisers and tiny detector (#373),
  rotated line crops as in PP-OCR v1 (#374), PP-DocLayout-S for region order on scans (#375), table structure on
  scanned pages against a model-free baseline (#376), and int8 models (#377). Server, large-layout, formula and
  vision-language models were rejected for size or speed; orientation classifiers fix no failing row.

## Throughput and memory

- **Benchmarks** (`hyperfine`, README):

  | Input | dijo | `tika-app` 4.1.0 |
  | --- | --- | --- |
  | 28-page PDF | 55 ms, 23 MiB | 4.0 s, 301 MiB |
  | DOCX | 6 ms, 17 MiB | 3.3 s, 213 MiB |

- **A static, in-process binary.** Tika, GraalVM and JNI were removed in 0.2.2. The static Linux binaries need no C
  library and run `FROM scratch`. Tesseract, the language data, the PP-OCR models and the HEIC/AVIF decoder are
  linked or embedded. No `tesseract` on `PATH` or `heif-dec` is ever run (#317).
- **Hot paths.** Some 75 targeted changes (#230–#304) remove per-cell, per-glyph and per-pixel allocations and
  repeated lookups, with output unchanged.
- **Fewer OCR passes.**
  - A page of a known language is read once instead of four or five times (#198), and an image is enlarged only when
    its lines are small (#199): old print takes a third less CPU.
  - The first image's language is detected from a strip of six lines, about a fifth less CPU on Latin scans (#218).
  - `invert_threshold=0` cuts 3.4% of OCR CPU on the corpus, and up to 27% on vertical Japanese (#331).

## Streaming and memory

What an extraction holds is bounded by its parser, not by its input, and every byte it allocates is charged to it.

- **Prepare, then write.** A `Parser` (`dijo-base/src/formats/mod.rs`) reads a document only as far as deciding
  whether it extracts it. It returns a `Prepared` closure that writes the XHTML, or a `Decline`: unread, read
  differently from Tika, failing as Tika fails, or written as no document. Nothing is written before the decision,
  and the closure keeps what was read, so nothing is read twice.
- **Declared footprints (#179).** `Footprint::Bounded` means the parser reads only bounded parts of its input in
  place (headers, indexes, one entry, record or page at a time) and reads a file of any size mapped.
  `Proportional` builds structures as large as the input and reads a large file mapped under the memory budget.
  Every format parser is `Bounded` except DIF and Markdown, which need whole-document state (529af4d4).
- **Checked on generated files.** `just test-large` (`dijo-cli/tests/integration/large.rs`) extracts 1 GiB of
  text, 512 MiB of CSV, UTF-16 text and DOCX, a 256 MiB mbox, PDFs of 50,000 pages and over 256 MiB, million-row
  XLSX sheets stored and deflated, a ZIP of deflated entries, ZIPs nested nine deep and a sparse 3 GiB WAV. Each
  must be read by the native parser and stay under 512 MiB peak RSS. Except the WAV, each must also give its text
  and its first output before half the run is over. A 512 MiB DOCX peaks at 78 MiB (0.2.7). `just test-soak`
  fails a server whose memory grows past 1.25× + 64 MiB over a long run (#305).
- **Input.** A file up to 64 MiB (`MAX_READ_WHOLE`) is read whole. A larger one is mapped read-only. A parser
  that reads all of it goes through `Paging`, which searches 16 MiB windows and releases the pages read with
  `MADV_DONTNEED` every 64 MiB (`dijo-base/src/input.rs`).
- **Embedded documents in place.** An entry stored whole is handed on as `Input::Within`, a slice of its parent
  that shares the parent's pages, not a copy. Only an entry stored in several pieces is joined into a spool.
- **Decoded streams are spooled.** Compressed content is decoded into a `Spool` (`spool_limited`,
  `dijo-base/src/compress/mod.rs`). It stays in memory up to 64 MiB, and beyond that goes to an unlinked
  temporary file that is mapped once complete. A stream whose container declares it larger goes to the file from
  its first byte, and a declared size reserves at most 1 MiB in advance (0069f0cc). Decoding stops at
  `decode_limit`: 256 MiB, or 100 bytes per compressed byte (Tika's zip-bomb ratio), whichever is more.
- **Output streams.** Parsers write XHTML as they read, and the budget is checked on each write. With one job,
  CLI output goes straight to standard output. With more, each input's output is spooled and emitted in input
  order. Server responses stream as the document is extracted. A PDF is recorded whole while its output stays
  under 16 MiB, so it can still decline before writing anything. Past that it is flushed page by page (`RECORDING_LIMIT`,
  `dijo-pdf/src/formats/pdf/recording.rs`). Pages that need OCR are rendered and read on one thread per processor,
  and each page's output is passed on in order (`dijo-image/src/ocr/pages.rs`).
- **Per format.**

  | Format | What is held at once |
  | --- | --- |
  | ZIP, 7z, RAR | One entry, decoded, spooled and parsed before the next (9c698eb5, e1d0d9c5) |
  | Parquet | A row group, read a page per column chunk at a time, at most 256 MiB of pages (`dijo-small/src/formats/parquet/`) |
  | Apple DMG | Coded chunks of up to 16 MiB decoded on demand, the last four kept (`apple_disk_image/disk.rs`) |
  | CAB, CHM | One LZX window per decoder, allocated once and kept across resets. A CHM page's frames go into one reused buffer (`dijo-base/src/compress/lzx/`, 46b03947) |
  | OOXML | A part whose size the package declares spooled straight to a file (a5d28d02) |
  | Word `.doc` | About 4 bytes per paragraph instead of about 80 plus a copy of its properties (d18f32a9) |
  | XPS, multi-page TIFF | One page laid out or decoded, written and dropped at a time (c287517c, e3cca64b) |
  | UTF-8 XML | Parsed in place through a window that grows a checked chunk at a time (3e712350) |
  | WARC, RSS/Atom | One record or entry at a time (e3dc87d5, d2d78dd0) |

- **A budget per extraction.** `CountingAllocator` (`dijo-base/src/memory.rs`) wraps the global allocator (mimalloc
  in the static build) and charges each allocation to its extraction. Allocations of 4 KiB or more are tracked by
  address, so a block freed on another thread credits the right budget. OCR threads charge the extraction that
  started them (`memory::share`). `--max-heap-bytes` sets the budget. Once it is overrun, that extraction fails with
  `MemoryLimit` and the next one starts afresh. One allocation may overrun the budget before the check.
- **C libraries are reserved explicitly.** Tesseract's memory is outside the allocator's view, so it is reserved at 4
  bytes per pixel for grey images and 11 for colour, measured from `tesseract`'s peak on 14-megapixel images, plus a
  byte per 3 pixels while a layout is kept (`dijo-image/src/ocr/tesseract.rs`). PP-OCR detection activations are
  reserved at 128 bytes per input pixel (115 measured) and UVDoc's at 48 (31–43 measured, `ppocr.rs`). An oversized
  page therefore fails cleanly.
- **OCR is what needs bounding.** One OCR reading peaks at 250–470 MiB. Documents without OCR peak at 15–25 MiB
  (#307).
  - No more readings run at once than the process has processors, however many documents are read
    (`dijo-image/src/ocr/slots.rs`, #305, #307).
  - Idle tesseract engines share one least-recently-used list of at most one engine per processor, whatever their
    model, and an engine idle for 5 minutes is freed. Before this, a soak run created 96 engines in four minutes,
    and an idle server held 1.5 GB of them (8b83d9f5).
- **Models.** Tesseract's models come in the OCR bundle, which is unpacked once into a cache directory
  (`dijo-native/src/lib.rs`) and then mapped (see `mmap-models` below). The PP-OCR and UVDoc models are embedded
  uncompressed, their weights split from the ONNX graph and aligned per tensor at build time
  (`dijo-native/build.rs`), and rten reads the weights where the program holds them, so every process shares one
  copy through the page cache and nothing is unpacked. An idle OCR worker of `serve --isolation process` holds
  about 73 MB less private memory (Private_Dirty from ~170 to ~97 MB), for a binary 6.7 MB larger.
- **Tesseract patches and build** (`ci/tesseract-patches/`, rationale in `ci/tesseract.nix`):

  | Patch or build change | Effect |
  | --- | --- |
  | `mmap-models` | Models are mapped read-only and their dictionaries (88.8 of `script/Latin`'s 89.4 MB) read in place, shared by every process through the page cache. Peak for a Latin page falls from 305 MB to 191 MB, about 85 MB less per worker (#211) |
  | `adapts-only-while-learning`, `lstm-skips-universal-ambigs` | No empty adapted class per character unless the legacy classifier learns, and no universal ambiguities loaded for LSTM models. A chi_sim engine takes 9 MiB instead of 43, eng 2 instead of 10, OSD 19 instead of 32 (#308, #311) |
  | `shares-page-layout` | Engines of compatible models recognise an image in a layout already found instead of thresholding and analysing it again. Layout analysis was 28% of corpus reading time, and a reread with another model now saves 400 ms (#334) |
  | `rows-restart-in-place` | Iterators return to the start of a row from where they entered it instead of walking the page from its start. A page of thousands of tiny words reads in seconds instead of hanging (#316) |
  | `takes-empty-pages-as-read` | An empty layout is not analysed again for every confidence query (#359) |
  | `inline-stridemap` | 10–13% faster per page. `-O3` adds about 5% |
  | `lstm-copies-in-place`, Pix word images, skipped fixed-pitch test | Up to a quarter fewer instructions per page, same text (#217, #219, #223) |
  | No OpenMP | One page per thread instead. An OpenMP team cost 2.6× the CPU for a 1.2× faster single page (#62) |

- **Processes.** A CLI batch of more than two parallel jobs runs in worker processes of two jobs each
  (`JOBS_PER_PROCESS`, `dijo-cli/src/batch.rs`). A worker that dies costs only its jobs, which run again one at a
  time. In the server, `--isolation process` confines a crash or runaway document to one request,
  `--worker-max-rss-bytes` caps each worker on Linux, and a worker whose extraction overran its timeout is replaced
  (0.2.7).
- **glibc allocator** (`dijo-cli/src/lib.rs`). Arenas are capped at the processor count (0.2.7). Blocks of 4 MiB or
  more are mapped on their own (0.2.11, #309). With glibc's dynamic threshold, an idle server held 2.58 GB of arenas
  for 47 MB in use. With the threshold fixed at 4 MiB (4ad636d0), peak RSS on a four-client server run fell from
  1700–1857 to 1271–1392 MiB, and idle RSS from 1581–1768 to 1101–1156 MiB, at no measurable CPU cost. 128 KiB cost
  8–12% more CPU.

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
- **On the quality corpus.** The lighthouse text rendered as DVI by LaTeX, as Matroska subtitles by mkvmerge, as
  ISO 9660, cabinet and RPM packages, as Parquet by DuckDB, as MOBI by calibre and as DXF is read by dijo with
  recall 1.000 and by Tika, Docling and Kreuzberg not at all. dijo also reads the AbiWord, SVG, SubRip, iCalendar and
  vCard files completely; Tika reads them too, with more spurious text on the calendar and the cards. dijo reads
  nothing from the PostScript dvips writes or from the Scribus 1.7 file (`niche-formats` rows of `COMPARISON.md`).
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
