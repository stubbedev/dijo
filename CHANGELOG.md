# 0.2.12 (2026-10-27)

### Added

* The server answers tika-server's `PUT /tika` and `GET /tika`, so a Tika client can switch to dijo unchanged.
  `PUT /tika` extracts plain text with the passwords of `Password` and the languages of `X-Tika-OCRLanguage`. It
  ignores the OCR and timeout tuning headers, since dijo decides those itself, and refuses any other `X-Tika-` option.

# 0.2.11 (2026-10-24)

### Changed

* Every build reads with the tesseract it carries and never with one found on `PATH`: the static executable links it,
  and every other build embeds the OCR bundle, which a build now requires (`DIJO_OCR_BUNDLE`; `just ocr-bundle`
  assembles one, and the Nix development shell provides one). A source build without it fails to compile, and HEIC
  and AVIF images are decoded by the bundled decoder, not `heif-dec` (#317).
* A document its parser fails on is the client's to fix: the server answers `422` with category `corrupt`, not `500`
  with `internal`, unless its cause says it is encrypted, too large, too slow or unsupported (#314).

### Fixed

* A page tesseract splits into many small words, such as a scan decoded to noise, no longer hangs paragraph
  detection, which walked the page from its start once a line: such a page now reads in seconds (#316).
* A damaged AV1 frame no longer aborts the process from inside the AV1 decoder: rav1d is pinned to its unreleased fix
  (#313).
* A server whose executable was replaced or deleted while it ran still starts workers, from the running image (#312).
* OCR uses less memory: no more readings and pages run at once in a process than its processors, idle tesseract engines
  share one bound list, an engine keeps no adapted classes or universal ambiguities its model never uses (a chi_sim
  engine takes 9 MiB instead of 43), and glibc maps blocks of 4 MiB or more on their own (#305, #307, #308, #309,
  #311).
* Small text beside photos is enlarged for OCR by the height of the runs shaped as lines of text (#306).
* XLSX sheets are searched for protection as they inflate, spooling none to disk (#247).

# 0.2.10 (2026-10-23)

### Fixed

* A damaged TIFF whose directory claims more tag values than the file holds no longer aborts the process
  reserving memory for them: what a TIFF's tags and compressed strips may allocate is bounded by the file's size.

# 0.2.9 (2026-10-23)

### Changed

More needless work removed from hot paths, with output unchanged (#268 to #304):

* HTML and XHTML: element names are compared without building them, attributes are filtered as they are read, and
  references resolve without building strings (#268, #270). The normaliser keeps its buffers across chunks (#269), and
  the server reads worker frames into one buffer (#271).
* OCR and images: an image with alpha is encoded for tesseract once, skew detection projects pixels through tables,
  pixels are converted, oriented and pasted without dispatching on each, and ICC colours convert without allocating
  (#272, #273, #276, #277). DjVu IW44 places coefficients by table and inverts columns a row at a time; JB2 keeps its
  pixel contexts in rolling registers (#274, #275).
* XML: known XHTML names are borrowed, SAX gets a start tag's declarations without copied attributes, and namespace
  prefixes resolve through a map (#278, #279, #280). OLE2 sizes a stream's loop detector by its chain (#281), and the
  fixed cell value formatters are built once (#282).
* PDF: an operator is described only when it fails, keywords are searched with memmem, stream data is copied only
  when a filter needs it owned, and a line's font runs merge through a priority queue (#283 to #286).
* Archives, mail and MIME: RAR filters run in the VM's memory, WARC's machines take a generic callback, magic regexes
  rule out regions by their first characters, OPC parts are keyed once, mail dates use a constant month table, and
  name extensions are looked up without collecting suffixes (#287 to #292).
* Office: Access text is decoded directly, workbooks no longer allocate per cell, DOC table rows take their start as
  read, and EPUB indexes its entries once (#293 to #296).
* MAT, DBF, OneNote, source code, FictionBook and ISA-Tab avoid per-value and per-line allocations and linear
  searches (#297 to #304).

# 0.2.8 (2026-10-23)

### Changed

Needless work removed from hot paths across the formats, with output unchanged (#230 to #267):

* JPEG: entropy-coded data is decoded through a bit buffer and a lookahead table, and each component is upsampled
  once and converted in place (#230, #231). OCR prepares an image once per orientation rather than once per retry,
  reusing its grey and analysis images (#232).
* Spreadsheets: whether a format is a date format is remembered, the powers of ten of General number text are
  computed once, and a cell's format is looked up once without allocating (#234, #235, #236). XLS strings are read as
  one slice and skipped without being built (#240, #241); XLSX and XLSB cells borrow their values, references and
  formats, and XLSB hands on a cell's column instead of formatting and reparsing its address (#245, #246).
* PDF: CMap codes are read without a buffer of their own, a glyph's text is resolved once, a code's CID range is
  found by search, and image samples and single-sample colours are converted through tables; bitonal images and
  CCITT black runs are written without per-pixel work (#252 to #258).
* AES is decrypted a run of blocks at a time (#237). Written text is checked and escaped by bytes, and the XML
  parser keeps non-ASCII text and attribute values on its fast path (#238, #239).
* DOC text is decoded straight into its output, RTF picture data a run at a time, and xls drawings are checked
  without inflating their metafiles (#242, #243, #244). DOCX runs keep their text buffer (#251).
* Plain text and CSV are written from the decoded pieces as they are, and text output splits XHTML a run at a time
  (#248, #249). With a cache, an input is hashed once (#250).
* Mail: base64 is decoded through a table and the boundary searcher is built once (#259, #260). MIME magic anchors
  are found when the tables are generated (#261); ZIP entry names are decoded once (#262); SQLite columns are
  classified once per table (#263).
* SAS column formats are classified once and decompressed runs filled as slices; MAT tag data is read in pieces;
  OneNote superseded objects are compacted in one pass; XMP array items skip a quadratic duplicate scan (#233,
  #264 to #267).

# 0.2.7 (2026-10-22)

### Added

* Blackletter (Fraktur) is read with tessdata's `deu_latf`, now bundled: a page whose reading shows the long-s
  signature and that detection reports as Fraktur is read again with it, and the reading with more words of the
  language is kept. Two 1895 Fraktur newspaper pages read with a character error rate of 0.057 and 0.162 instead of
  0.118 and 0.213 (#225).
* A PDF's first page read by OCR starts in the language its doubtful text layers show or its `/Lang` declares.

### Changed

* With the automatic OCR language, the first image of a file is read once, in the language of a strip of six of its
  lines, rather than whole in `script/Latin` and again: about a fifth less CPU on Latin-script scans (#218).
* Tesseract is built to keep word images as Pix rather than round-trip them through PNG (#217), its LSTM writes
  convolution windows and int8 time steps in place (#223), and it skips its fixed-pitch test (#219): a page reads with
  up to a quarter fewer instructions and the same text. A test fails when tesseract selected no SIMD dot-product
  kernel (#224).
* Rotating and resizing an image for OCR reads its pixels without matching its kind for each one: a skewed scan takes
  about a fifth less CPU (#216). Dehyphenation copies only words it has not seen, about 15% less CPU on long
  documents (#215).
* The command line holds glibc's allocator to as many arenas as the process's processors, so a long-running server's
  memory stays level.

### Fixed

* A page under uneven lighting has its grey image divided by its background, so the recogniser no longer reads the
  shadow: a page under a sharp-edged shadow reads without errors instead of a character error rate of 0.301 (#227).
* A large OOXML package is opened at the offset its end record gives instead of being scanned, so a 512 MiB DOCX peaks
  at 78 MiB again instead of 526 MiB.
* A worker process whose extraction overran its timeout is replaced, so the parse it left behind can no longer keep
  allocating.

# 0.2.6 (2026-10-20)

### Added

* OCR restores the long s of early modern print, which tesseract reads as f (`fu cafa` for *su casa*), by the
  dictionary of the page's language model, on a page with at least three such words (#210).

### Changed

* Tesseract reads its models' dictionaries in place in the mapped model file, so every process shares one copy of
  them: reading a Latin page peaks at 191 MB instead of 305 MB, and each worker process holds about 85 MB less (#211).
* With the automatic OCR language, a page of a known language is read once rather than four or five times (#198), a
  page waits for the pages before it rather than guessing their script, and an image is enlarged only when its lines
  are small, not its specks (#199): pages of old print read with a third less CPU and a lower peak.

### Fixed

* Scans set in columns read column by column: tesseract is told the resolution an image records or was rendered at,
  and no longer runs a column's lines into the title beside it (#209).
* `--format pages` learns running headers and footers from every window of pages instead of the first eight, from left
  and right pages apart, and despite OCR misreadings, so a book that opens with front matter has them moved out (#201);
  a page that is nothing but its running line keeps it as its text (#213).
* The blocks of an OCR'd page are read from the top down where they share width, so a verse above prose comes first
  (#200).
* Chinese, Japanese and Thai scans are read without a space between every character (#205), a Thai scan is no longer
  dropped as noise (#203), and a Hebrew scan is read in Hebrew rather than Thai (#204).
* A PDF page that scales its text through the CTM, as macOS Quartz writes it, reads its lines apart instead of
  interleaving them (#212).
* An empty table cell written as `<td/>`, as spreadsheets write them, keeps its tab and no longer joins the next
  heading to the following text when splitting pages or elements (#214).

# 0.2.5 (2026-10-20)

### Fixed

* No reading depends on the machine's time zone any more: property list dates and MSG submission dates are read in
  UTC (#193).
* Numbers are no longer wrapped the way Java's `int` and `long` wrap them: a PDF number that no `int` holds counts as
  missing rather than wrapping around, and a date past what a `long` of milliseconds holds is no date (#193).
* An unmappable Big5 character in a Word 6/95 document is U+FFFD rather than `?` (#193).
* An XML document switches to its declared encoding right after the declaration (#193).
* Excel formats are written as Excel shows them: `0.##` writes 5 as `5.`, times are rounded to the fraction of a
  second the format shows, and Japanese eras (`ggge`) and the `B1` calendar prefix are written (#193).

# 0.2.4 (2026-10-20)

### Added

* OCR models for every national language of Europe in the release binaries. Without `--ocr-lang`, each page is read
  again with the model of the language detected in it, so a Spanish scan reads with `spa` rather than `script/Latin`.

### Fixed

* `--format pages` writes a record for every page, with empty text for a blank one, instead of skipping it (#196).
* `--ocr-lang` (and the server's `ocr_lang`) with a model that is not installed is a usage error (exit 2, HTTP 400)
  listing the installed models, rather than every page failing as an internal error; OCR errors no longer name Java
  exception classes (#195).
* A page without text, such as a patterned endpaper, no longer comes out as OCR noise in another script: a reading
  with low confidence that is mostly digits and marks, or in no detected language, is dropped and reported as
  `"noise":true` in `metadata.ocr` (#194).

# 0.2.3 (2026-10-20)

### Added

* Pack200 archives (`.pack`, `.pack.gz`): unpacked natively into the jar they were packed from, so
  every format Tika reads is now read.

# 0.2.2 (2026-10-20)

### Renamed to dijo

Migration from extractous:

* Binary `extractous` is now `dijo`; crates `extractous-*` are `dijo-*` (`extractous` CLI crate is `dijo`).
* Environment variables `EXTRACTOUS_*` are `DIJO_*` (`DIJO_FORMAT`, `DIJO_JOBS`, `DIJO_PASSWORD`, `DIJO_OCR_BUNDLE`, ...).
* Server headers `X-Extractous-Password` and `X-Extractous-Filename` are `X-Dijo-Password` and `X-Dijo-Filename`.
* Metadata keys `extractous:*` are `dijo:*`; parser names `extractous::*` and `ai.yobix.*` are `dijo::*`.
* Prometheus metrics `extractous_*` are `dijo_*`; the container image is `ghcr.io/stubbedev/dijo`.

### Removed

* Apache Tika, GraalVM and JNI: every format is read natively, and neither building nor running needs Java.

### Added

* HWP 5 (Hancom Office) documents, including distribution documents.
* VBA macros from DOCX, XLSX, XLSB and PPTX with `extract_macros`.
* Multi-valued MSG properties; MSG streams that end early are read as far as they go.
* Access databases where one OLE object or system catalog is damaged are read around it.
* Excel number formats as Excel displays them: quoted text in dates, elapsed times, `?` placeholders,
  thousands scaling, engineering notation, accounting zero sections.

### Fixed

* Ports no longer reproduce JVM accidents: entries come out in document order instead of Java
  hash-map order (PDF CMaps and encodings, PowerPoint, mail, media, EPUB, ...), malformed text decodes
  as WHATWG specifies, and a FLAC duration overflow, a lost UTF-16 BOM character and wrapping XLSX
  cell references are fixed.


# [](https://github.com/yobix-ai/extractous/compare/v0.1.5...v) (2024-10-30)


### Bug Fixes

* add criterion benchmarks ([5858356](https://github.com/yobix-ai/extractous/commit/585835623bb7c098e4359636d8d028af4835d699))
* fixed issue 16 and added test case ([03062be](https://github.com/yobix-ai/extractous/commit/03062beca1eceede16878707dc2647b71848b65e))
* minor fix to build script ([8ac14b8](https://github.com/yobix-ai/extractous/commit/8ac14b8d36c8a8d733c08a123686d02c52bcf5bb))



## [0.1.5](https://github.com/yobix-ai/extractous/compare/v0.1.4...v0.1.5) (2024-10-29)


### Bug Fixes

* don't copy libs to same dir ([e5dad2f](https://github.com/yobix-ai/extractous/commit/e5dad2f7434b8cae58b6aa61c0c66165473aa35f))
* don't create graalvm archive if download fails ([2c84a50](https://github.com/yobix-ai/extractous/commit/2c84a5005bbea993d57e187a1f7f7f73ca8c3f87))
* don't include static lib on windows + bump release version ([02ed47f](https://github.com/yobix-ai/extractous/commit/02ed47fd79572acc671309188adb8dcb2995592a))


### Features

* implemented windows support + updated readme with ocr ([17b5663](https://github.com/yobix-ai/extractous/commit/17b5663b4f852396646ec43c03f0fb888825d750))



## [0.1.4](https://github.com/yobix-ai/extractous/compare/v0.1.3...v0.1.4) (2024-09-18)


### Bug Fixes

* remove unenessary liftimes ([b321df6](https://github.com/yobix-ai/extractous/commit/b321df6e743273ed02b229cf152b9554f610914e))
* tolerate invalid utf8 chars when converting to string ([24a45e7](https://github.com/yobix-ai/extractous/commit/24a45e79740ad69aca3bdddf182aa56b64640965))


### Features

* add buffer to JReaderInputStream and reuse it to not call new_byte_array on every read ([881d919](https://github.com/yobix-ai/extractous/commit/881d9190f7047163a497d2b2e578eca1f7b890ef))
* refactor examples ([8a7f9a3](https://github.com/yobix-ai/extractous/commit/8a7f9a3ba7b5db4e07a07efbe516dfbdcd52f79d))



## [0.1.3](https://github.com/yobix-ai/extractous/compare/v0.1.1...v0.1.3) (2024-09-11)



## [0.1.1](https://github.com/yobix-ai/extractous/compare/a86462cc49b77f9e17b8faade80edaf679edf225...v0.1.1) (2024-09-11)


### Bug Fixes

* add missing jni entry for parseToString method ([21cf299](https://github.com/yobix-ai/extractous/commit/21cf299cc4d3c952d7ebaa793b9a5c4e16069d64))
* add more reahability data ([fd2cb3a](https://github.com/yobix-ai/extractous/commit/fd2cb3ad0a1af19814350d61b533117273f65a8a))
* move lib artifacts to deps folder ([b150ca9](https://github.com/yobix-ai/extractous/commit/b150ca9f316352b488eff06904555c0d0620c6ce))
* return StreamReader when extracting ([b20209a](https://github.com/yobix-ai/extractous/commit/b20209a9e49c19565fabe6ead799519d5582e010))
* update reachability data ([77a0aae](https://github.com/yobix-ai/extractous/commit/77a0aaef42beb1fe61dd2b93aad9a1c24a1b74c8))
* update tika-native ([e009af9](https://github.com/yobix-ai/extractous/commit/e009af9a942e9e861ae985d6629356c89dd28534))


### Features

* add jni helper functions to print to stderr in case of java exception is thrown ([2b7d641](https://github.com/yobix-ai/extractous/commit/2b7d641e137818681bca63c0f3c156e4c76dbd09))
* add StreamReader struct and make JReaderInputStream takes a GlobalRef ([4bfa895](https://github.com/yobix-ai/extractous/commit/4bfa895b7f97c4a5467e0ae3fd6c252f272e3b03))
* add tika-native source files ([78344e8](https://github.com/yobix-ai/extractous/commit/78344e88d7e9b14478ea83196659f9ab0c381a12))
* better error reporting ([1e96cf5](https://github.com/yobix-ai/extractous/commit/1e96cf525ec56aa362904fbb71c914684027d5a1))
* implemented extracting to a streamed reader ([b6533be](https://github.com/yobix-ai/extractous/commit/b6533be5507f3513e17d4677b477e6e6a6cc6bc6))
* initial integration of tika-native into rust ([579e6cd](https://github.com/yobix-ai/extractous/commit/579e6cd73d53088feca5f41be80dab4026d485f8))
* initial project sturcture ([a86462c](https://github.com/yobix-ai/extractous/commit/a86462cc49b77f9e17b8faade80edaf679edf225))
* introduced extractor builder pattern ([6d091f8](https://github.com/yobix-ai/extractous/commit/6d091f84c70a2f4338921e6c801bc0b4a8942603))
* parseFile with all config wrapper objects ([a7c39c4](https://github.com/yobix-ai/extractous/commit/a7c39c43d37cb92e63b1714264898c733b0ea6f5))



