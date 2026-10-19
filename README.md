# Dijo

Extract text and metadata from documents on the command line, without Java.

This is a fork of [yobix-ai/extractous](https://github.com/yobix-ai/extractous), which ran
[Apache Tika](https://tika.apache.org/) compiled to native code with GraalVM. Documents are now read by native Rust
parsers, ports of Tika's that aim at its output. Neither building nor running dijo needs Java, GraalVM or JNI.
Tika remains only as the reference conformance and the comparisons measure dijo against (see
[Development](#development)).

## Install

Each [release](https://github.com/stubbedev/dijo/releases) has archives for Linux (x86_64, aarch64, and static
builds for both, `x86_64-linux-static` and `aarch64-linux-static`), macOS (aarch64) and Windows (x86_64) with the
binary, its licence and the third-party notices, plus `SHA256SUMS`, a CycloneDX SBOM of the Rust crates, and build
provenance you can check with `gh attestation verify <archive> --repo stubbedev/dijo`.

To build from source instead:

```sh
cargo install --locked --git https://github.com/stubbedev/dijo dijo
```

A document no native parser reads yet fails with an `unsupported` error naming why (no native parser for its type, or
what of it the native parser does not read yet); an embedded document of that kind is reported unread in its
container, which is extracted.

The release binaries other than the static ones link the system's C library (glibc 2.35 or later for the Linux
release binaries), so a container needs a base image with it, such as `debian:bookworm-slim`. Nothing else needs
installing.

The `x86_64-linux-static` and `aarch64-linux-static` release binaries need nothing at all, not even a C library: they
run in a `FROM scratch` container. They are the program built for musl, with tesseract, Leptonica and libheif (with
its HEVC and AV1 decoders) linked in and the OCR language data embedded. `just static-native` builds one on Linux into
`target/static/dijo`: the flake's `static` output, built with Crane, whose `container` output is the image
around it and `static-dependencies` the third-party crates alone, which a binary cache serves to CI and fresh
machines.

OCR runs tesseract in the process: linked into the static build, and loaded by every other release binary from a
library it carries, built from tesseract's pinned source with vcpkg's libraries at a pinned commit and linked
statically (the static build's comes from nix), with the language
data the automatic language needs (orientation and script detection, tesseract's `script/Latin`, `script/Greek` and
`script/Cyrillic` models, which read every language of those scripts with its letters and diacritics, and a fast model
for each other script it reads; listed in `ci/tessdata.tsv`), so they need nothing installed beyond the system's C library: a binary built with
`DIJO_OCR_BUNDLE` pointing at a bundle from `just ocr-bundle` does the same (about 90 MB larger, 170 MB once
unpacked), and `--ocr-lang` can name only the bundled models there. Reading Danish, Swedish, Norwegian, German, Polish,
Czech or Turkish scans with the English model instead loses 3 to 13% of the characters, the diacritics.
Other builds run `tesseract` from `PATH`, with its own language data. Without `--ocr-lang`,
each image is first read as upright text in Latin script, the most common kind. Only when tesseract's mean confidence
in that reading is 80 or less (upright Latin pages read at 90 or more; other scripts, and pages upside down, at 60 or
less) does tesseract's orientation and script detection run (the `osd` data, which distribution packages install with
tesseract), stopping once it is sure of ten characters rather than fifty, and finding nothing when no script is likelier
than another: the image is turned upright and read again with the installed model for its script, such as
`script/Arabic`, then `ara`, falling back to `eng`. Detection reports Georgian pages as Malayalam, so after a Malayalam
report the page is read as Georgian first. It also takes upright Armenian for upside-down Latin, so when no reading of a
page with text is confident, Armenian, Georgian and the reported script are tried as well (each at most once) and the
most confident reading is kept. Where tesseract runs in the process, each of these readings is a guess that tesseract gives up once the mean
confidence of its first 20 words is below 70 (the Latin model reads the first words of a Cyrillic, Greek, Armenian or
Georgian page at 55 or less, and of a Latin page at 88 or more), so a page in another script costs little more than
one reading in the right one; when every guess is given up, the page is read to the end as detection found it. With `--ocr-lang`, a page is read in that language, and detection runs only to turn a page
tesseract was not confident of. Tesseract segments pages without its own detection (page
segmentation mode 3, where Tika runs mode 1), since pages reach it upright. The pages of a multi-page image or scanned
PDF are read side by side, on as many threads as the process has processors. Each later page is read in the script the
first page settles on, and read again only when that differs from the script assumed when it started. So a page reads
the same whichever finishes first, and a two-page scan takes about one page's time.

Under `--ocr auto`, each PDF page's text layer is checked before it is used. A page with next to no text is OCRed
as a scan. A page whose text layer is garbage is OCRed and its text replaced. Garbage here means private-use or
control characters, symbols where letters should be, words mixing alphabets, too few vowels for a language written
in Latin script, Arabic words split into one-letter fragments, or mostly unmapped glyphs (broken `ToUnicode` maps,
custom font encodings). A page that is only
doubtful is OCRed as well and keeps whichever text reads better. The JSON metadata lists each replaced page and the
reason under `rejected_text_layers`. Clean pages are never OCRed. Every image or page OCR reads gets a report under
`ocr` in the JSON metadata: the model it was read with, the script detected, and the EXIF orientation, inversion,
despeckling, turn, deskewing, enlargement and thresholding applied to it first.

## Usage

```sh
dijo report.pdf                         # plain text to standard output
dijo -f xhtml report.docx               # Tika's XHTML
dijo -f json -m *.pdf                   # one JSON object per file, with metadata
dijo -r -o out/ archive/                # one output file per input, mirroring archive/
cat scan.tiff | dijo --ocr-lang deu -   # standard input, OCR in German
dijo -f pages manual.pdf                # one JSON line per page, for search indexing
dijo detect mystery.bin                 # application/pdf
```

| Option | Meaning |
| --- | --- |
| `-f, --format text\|xhtml\|json\|pages\|elements` | Plain text (default), Tika XHTML, one JSON object per input, or one JSON line per page or per element |
| `--raw-metadata` | Add Tika's own metadata keys to json output |
| `--no-normalize <STEP,...>` | Leave out normalisation steps: `invisible`, `ligatures`, `soft-hyphens`, `unicode`, `whitespace`, `headers`, `hyphenation`, `reading-order`, `bookmarks` |
| `--raw` | No normalisation at all: the text exactly as Tika extracted it |
| `--password <PW>`, `--password-file <FILE>` | Passwords tried, in order, on encrypted documents (the file or `DIJO_PASSWORD` keeps them out of process listings) |
| `--timeout <SECONDS>` | Per-input limit; the text extracted so far is kept and the input reported as partial |
| `--embedded inline\|separate` | Add embedded documents' text to their container's (default), or extract each as its own document, recursively |
| `--max-depth`, `--max-embedded`, `--max-expanded-bytes` | Limits for `--embedded separate`: nesting depth, number of embedded documents, and bytes they may expand to |
| `--cache-dir <DIR>`, `--cache-max-size <BYTES>` | Reuse the output of complete extractions while the input, the options and the dijo build are unchanged; evict least recently used entries beyond the size (also for `serve`) |
| `--cache-partial` | Also cache partial results (never those cut short by a timeout) |
| `--cache-trust-mtime` | Skip rehashing inputs whose path, size and modification time match the last run |
| `--max-heap-bytes <BYTES>` | Cap the memory an extraction may allocate; an extraction that needs more fails with `limit-exceeded` and the next one runs normally |
| `--spool-dir <DIR>` | Where temporary files go: spooled input and output, embedded documents, decoded images |
| `--strict` | Fail an input when any part of it could not be extracted, not only when nothing could |
| `-m, --metadata` | Include metadata: on standard error for text and xhtml, inline for json |
| `-o, --output <DIR>` | Write one file per input into DIR, mirroring the input paths |
| `-r, --recursive` | Descend into directories, in sorted order |
| `-j, --jobs <N>` | Inputs extracted in parallel (default: the number of cores). Past two, they run in worker processes of two each, since extractions sharing one isolate slow each other down: 120 copies of a 28-page PDF take 3.2 s on 12 hardware threads, against 6.4 s in one process, at about 130 MiB per worker process. A worker process that crashes costs only the document it was extracting alone. Output stays in input order |
| `--ocr auto\|never\|always` | OCR images, and PDF pages without text or with a garbage text layer (default); never; or every PDF page as well |
| `--ocr-lang <LANGS>` | Tesseract languages, such as `deu` or `eng+ara`; detected per image from its script by default |

Every option has an environment variable equivalent (`DIJO_FORMAT`, `DIJO_JOBS`, ...); `dijo --help`
lists them. Errors go to standard error with the input path and the full cause chain, and one failed input does
not stop the others. Damage inside an input (an unreadable attachment, a broken page) does not discard the rest of it:
everything that extracted is written and the failures are reported. The exit status is 0 when text was extracted from
every input, 1 when nothing could be extracted from at least one (with `--strict`, when any part of one failed) and 2
on a usage error.

Text, json, pages and elements output is normalised for indexing, so the same word produces the same token however the
document encoded it: control characters and zero-width spaces are dropped (joiners that scripts need are kept),
ligatures such as `ﬁ` become `fi`, soft hyphens are dropped, characters are composed to Unicode NFC, exotic spaces
become plain ones, runs of spaces collapse, lines end with `\n` without trailing spaces, and at most one blank line
appears in a row. Tabs are kept, since they separate table cells. In `--format pages`, lines that recur at the top or
bottom of most pages (running headers and footers, page numbers) move out of the page text into the page record's
`header` and `footer`; they are learned from the first 8 pages, which are held back meanwhile. Text a tagged PDF marks
as page furniture rather than content (its running headers, footers and page numbers) moves into the record's
`artifacts` instead. Text and json output drop both. A word hyphenated
across a line break (`extrac-` / `tion`) is joined. The hyphen stays only when the text before shows a hyphenated
compound: the word already holds a hyphen (`state-of-the-` / `art`), or `wellknown` did not occur but `well-known` or
both `well` and `known` did. With no evidence either way the word is joined, so a compound that breaks at its hyphen
before either half has occurred loses the hyphen. A hyphen before a capital or a digit, or before a blank line,
stays where it is. The bookmarks (outline) Tika appends to a PDF's text, which repeat its headings, are left out.
Normalisation streams, so it holds
back at most a few characters, or a word after a hyphen that ends a line. XHTML output is never normalised.

With `--cache-dir`, a run over files that have not changed only hashes them and copies their stored output. An entry is
keyed by the BLAKE3 hash of the input's content and its name, the options that shape the output, the dijo
executable itself (so a new build or embedded library invalidates everything) and the tesseract version.
Only complete extractions are stored, each written to a temporary file and renamed into place, so concurrent runs and
crashes never leave a broken entry; once the cache grows past `--cache-max-size`, the least recently used entries are
evicted. With `--embedded separate`, every embedded document without embedded documents of its own is an entry of its
own, keyed by its own content and file name and replayed with the provenance of wherever it turns up, so the same
attachment in many emails is extracted once; the emails and archives around it are read again, which is cheap. The
cache is not available with passwords, so decrypted text never lands on disk. `dijo serve` takes the same
options: a repeated upload is answered from the cache without extracting it again, requests with passwords bypass it, and
`/metrics` counts hits and misses.

## Server

```sh
dijo serve --listen 0.0.0.0:9998 --workers 8
curl --data-binary @report.pdf -H 'X-Dijo-Filename: report.pdf' localhost:9998/extract
curl --data-binary @report.pdf 'localhost:9998/extract?format=json&metadata=true'
curl --data-binary @mystery.bin localhost:9998/detect
```

| Endpoint | |
| --- | --- |
| `POST /extract` | The request body is the document. Query: `format` (`text`, `xhtml`, `json`, `pages`, `elements`), `metadata`, `raw_metadata`, `raw`, `no_normalize` (comma-separated steps), `ocr`, `ocr_lang`, as on the command line. Passwords go in `X-Dijo-Password` headers, an optional file name hint in `X-Dijo-Filename`; without one, a `Content-Type` other than `application/octet-stream` is the hint. |
| `POST /detect` | The detected media type, as text. |
| `GET /healthz`, `GET /readyz` | Liveness, and whether a request would be accepted now. |
| `GET /metrics` | Prometheus metrics: requests by endpoint and status, failures by category, extraction time per media type, bytes in and out, workers and requests in flight. |

Extraction runs on a fixed pool of worker threads (`--workers`, by default the number of cores up to 4: the threads
share one isolate, and more of them slow each other down). Uploads are spooled to disk and the response streams as
it is extracted.
When every worker and queue slot (`--queue`) is busy, requests get 503 with `Retry-After`. A document that cannot be
extracted at all gets a JSON error document with 415 (unsupported), 422 (corrupt or encrypted) or 500; partial results
come back as 200 with their status in the JSON. `--max-heap-bytes` caps each extraction's memory (an extraction beyond it gets 413), `--max-input-bytes` caps uploads (413) and `--timeout` bounds a whole
request (408 while uploading, 504 before extraction starts, a cut connection after). `--max-output-chars` cuts a
document's text short (json, pages and elements report it as partial with a `limit-exceeded` error; XHTML stays
well-formed), and `--max-depth`, `--max-embedded` and `--max-expanded-bytes` bound `embedded=separate` as on the
command line. SIGTERM stops accepting and waits
`--shutdown-grace` seconds for running requests.

`--isolation process` runs each worker as a process of its own instead of a thread, so a parser that crashes, hangs
or runs away with memory takes down one request and not the server. The server restarts the worker for the next
request. A crashed worker fails its request with 500. A worker still running five seconds past `--timeout` is killed
(504). On Linux, `--worker-max-rss-bytes` kills a worker whose resident memory grows past the limit (413). Each
worker process holds its own memory, so `--workers` defaults to one per core here, and memory use
grows with it. With 4 workers on a mixed office and PDF
corpus, process isolation took 565 MiB at peak against 163 MiB for threads. It served 243 requests/s against 202 at
4 concurrent requests, and 37 against 52 one at a time.

### Security

Documents and requests are untrusted input. What the server defends against, and how:

- **XML attacks.** No parser resolves external entities or loads external DTDs (XXE), and entity expansion is
  capped, so a billion-laughs document fails as `limit-exceeded`. Tests cover XML, SVG and DOCX.
- **Resource exhaustion.** `--max-input-bytes` caps uploads, `--timeout` caps a request's time, `--max-heap-bytes`
  caps each extraction's memory, `--worker-max-rss-bytes` caps a worker process's memory,
  and `--max-output-chars` caps a document's text. Embedded documents have depth, count and expanded-size limits
  with `embedded=separate`; inline, Tika's zip-bomb guard applies.
- **Slow and idle clients.** `--client-timeout` closes a connection whose headers arrive too slowly, and answers an
  upload that goes silent with 408.
- **Busy servers.** Requests beyond the workers and `--queue` are refused with 503, never queued without bound.
- **Crashing parsers.** With `--isolation process`, a parser that crashes or hangs takes down one request. Its
  worker is restarted.
- **Paths from documents.** A client's file name hint is reduced to a bare file name inside the request's own spool
  directory, and embedded document names never become paths outside it.
- **Server-side requests.** The server fetches nothing on a client's behalf: it extracts only the uploaded bytes, so
  there is no SSRF surface. Passwords are never cached or logged.
- **Damaged files.** A nightly job extracts randomly damaged copies of the test documents (`just test-fuzz`) and
  fails on any crash, hang or missing answer.
- **Hostile markup and requests.** The parsers that read what a document or client controls (the XHTML splitter, the
  text normaliser, the query string decoder and metadata normalisation) are fuzzed nightly with coverage guidance
  (`just fuzz`, cargo-fuzz on stable Rust), each against a property it must keep: never panic, bounded output, the
  same text however it arrives in chunks.

What it leaves to the deployment:

- **Access control and transport.** There is no authentication and no TLS: put the server behind a reverse proxy or
  inside a private network.
- **Abuse limits.** There are no per-client rate limits, and no cap on idle connections beyond `--client-timeout`.
- **In-process parsing.** With the default `--isolation thread`, a crash in native code inside a parser ends the
  whole server.
- **Error detail.** Error responses carry the cause chain of the failure, but no stack traces.

### Container

Each release publishes a multi-platform image (linux/amd64, linux/arm64) built `FROM scratch`: the static executable
and nothing else but a `/tmp` and a cache directory, running the server as an unprivileged user (uid 10001). OCR reads
with the models the executable bundles (`ci/tessdata.tsv`):

```sh
docker run --rm -p 9998:9998 ghcr.io/stubbedev/dijo:latest
docker run --rm -v "$PWD:/data:ro" ghcr.io/stubbedev/dijo:latest /data/report.pdf
```

`just static-native` and then `just container` build the same image locally as `dijo:local`. `--ocr-lang` can name
only the bundled models.

## JSON output

`--format json` writes one `"kind":"document"` object per input per line, following the versioned schema in
[`schema/output-v1.schema.json`](schema/output-v1.schema.json):

```json
{"schema_version":1,"kind":"document",
 "source":{"path":"report.pdf","size":266240,"blake3":"0b9b01a7..."},
 "text":"...",
 "mime_type":"application/pdf",
 "status":"complete",
 "errors":[],
 "metadata":{"title":"...","authors":["..."],"created":{"utc":"2022-07-29T10:03:21Z"},"page_count":28}}
```

`--format pages` writes one record per page (`{"kind":"page","page":3,"text":...}`; slides count as pages, and a format
without pages is one record), and `--format elements` one per heading, paragraph, list item or table
(`{"kind":"element","element":"heading","level":1,"page":3,"text":...}`), each as soon as it is complete, followed by a
`"kind":"document"` record carrying the status, errors and metadata that are only known at the end.

`detected_language` is the language detected from the text itself (ISO 639-3 code, script, confidence and whether
it is `reliable` enough to act on, such as to pick a search analyzer), from the first 64 KiB of a document and from
each page's own text in `--format pages`. It is separate from `metadata.language`, which is what the document
declares. Detection starts from a Rust port of Apache Tika 4's CharSoup detector, with Tika's model of 204 languages,
and improves on it in two ways. A prior makes a national language more likely than the regional languages, dialects
and historical and constructed languages CharSoup takes them for (Zazaki for Turkish, Swiss German for German, Walloon
for French, Corsican for Italian). And where CharSoup's leader belongs to a group of closely related national
languages (Danish, Norwegian Bokmål and Nynorsk, Swedish, Icelandic and Faroese; Croatian, Serbian and Slovene;
Czech and Slovak; Russian, Ukrainian, Belarusian, Bulgarian, Macedonian and Serbian in Cyrillic; Spanish, Portuguese,
Galician and Catalan; German, Dutch, Luxembourgish and Afrikaans; Lithuanian and Latvian), a model of that group
trained on Wikipedia (CC BY-SA 4.0; `just language-groups`) decides between them. It also recognises Serbian written in Latin
script, which Tika's model only knows in Cyrillic and reports as Croatian.

On 200 Tatoeba sentences of each European language the test suite checks that every language is detected at least
as often as Tika detects it, alone and in runs of five sentences; on average 86.7% of the sentences against Tika's
80.5%, and 98.6% of the runs against 97.1%. Danish is right in 79.5% of the sentences (Tika 67.5%), Norwegian Bokmål
in 83.0% (68.5%), Swedish in 83.5% (71.0%), Turkish in 83.5% (67.0%), and all four in every run of five.
`just language-accuracy <tsv>` compares both on other texts.

`metadata` (with `-m`) holds normalised fields filled from whichever Tika key a format uses: dates are RFC 3339 in UTC,
or marked `unknown_offset` when the document gave no offset; multi-valued fields are always arrays. Tika's own keys are
available with `--raw-metadata`. `status` is `complete`, `partial` (some text before an error), `encrypted` (no password opened it) or `failed`, and
every error carries a category and its full cause chain. Decrypted documents report `"encrypted":true,"decrypted":true`
in `metadata`; passwords never appear in output or errors. Passwords decrypt PDF (a PDF with only an owner password
needs none), DOCX/XLSX/PPTX, Word, Excel and PowerPoint 97-2003, Access and 7z. Tika cannot decrypt ODF documents or
encrypted ZIP entries; those are reported as `encrypted`, and an encrypted entry leaves the rest of its archive
extracted with status `partial`. S/MIME-encrypted mail and OpenPGP data, binary or ASCII-armored, are reported as
`encrypted` too (decrypting them needs a private key); S/MIME-signed mail extracts normally.

### Embedded documents

With `--embedded separate` (or `embedded=separate` on the server) every document inside an input (archive members,
email attachments, objects embedded in Office files, images in documents) is extracted as a document of its own,
depth first, right after its container. Each record names the input in `source.path` and the document's place inside
it in `source.embedded_path` (`mail.eml/numbers.xlsx/image1.png`), with its size and BLAKE3 hash. A document whose
content already appeared in the same input is reported once more only as `"duplicate_of"` its first occurrence. In
text output each embedded document starts with a `=== path ===` line. Exceeding a limit skips that subtree with a
`limit-exceeded` error instead of failing the input.

## Quality

`quality/` holds a corpus generated from known text (`quality/truth`): the same documents as ODT, DOCX, DOC, RTF, PDF,
HTML and EPUB, two-column PDFs (one drawn right column first), a report with running headers and footers, Arabic,
Hebrew, Chinese, Japanese (also written vertically), Thai and Hindi documents, a PDF, a DOCX and a 300 dpi scan in
each national language of Europe (and Armenian, Georgian and Azerbaijani), plain text in the legacy encodings each of
them was written in (Windows code pages, ISO 8859, KOI8), tables, Word and OpenDocument
files with tracked changes, a comment and hidden text, a document with a footnote, a sidebar and a captioned table, a
PDF hyphenated by TeX, PDFs with broken text layers, and scans at 300 and 150 dpi, skewed and rotated. Every extraction
is scored against its truth: character and token error rate, recall (the share of the truth's words that were extracted
at all), spurious text and paragraph order. The test suite fails when a score gets worse than `quality/baseline.json`
allows, so CI holds every change to it.
[`quality/SCOREBOARD.md`](quality/SCOREBOARD.md) has the current scores; `just quality` refreshes it,
`just quality-bless` accepts new scores and `just quality-corpus` regenerates the corpus.
[`quality/COMPARISON.md`](quality/COMPARISON.md) (`just quality-compare`) scores tika-app of the same Tika version on
the JVM, Kreuzberg and Docling (run in Docker) alongside, each with its default options: over the documents extracted
with default options, dijo recalls 0.996 of the truth's words, Tika 0.848, Kreuzberg 0.738 and Docling 0.744, at
a token error rate of 0.006 against 0.199, 0.335 and 0.278. Tika scores better on no document of a European language;
of all the corpus, only on the recall of a two-column PDF, where it keeps a line-end hyphen that dijo joins. A document an extractor cannot read, such as a DOC or RTF
file or legacy-encoded text for Docling, counts as empty.

What it shows today: born-digital Office, OpenDocument and PDF files and scans (clean, skewed, rotated, or
photographed upside down with an EXIF orientation) extract exactly or within a few characters. An Arabic PDF whose
text layer splits words where their letters stop joining is read by OCR instead, which still misreads some words.

An untagged PDF page is read in reading order, not in the order its content stream draws the text: columns left to
right (right to left on a page of right-to-left text), a title before the columns it spans, a block above the blocks
below it. A page drawn in reading order already keeps its order, and so does a page with text in more than one
direction. A page written vertically, as Japanese and Chinese often are, is read column by column from the right,
each column a line, where Tika wrote every glyph on a line of its own. Tagged PDFs are read in the order of their
structure tree, and a tagged page written vertically still comes out a glyph per line. This is the `reading-order`
normalisation step, so `--no-normalize reading-order`, `--raw` and XHTML output keep the content stream order.

Before OCR, images and rendered PDF pages are turned upright: by their EXIF orientation, and by the skew of their text
lines, estimated from projection profiles. Light text on a dark ground (more than 85% of the image darker than its Otsu
threshold) is inverted first, so tesseract reads each line once rather than again inverted, and the page's text lines
can be measured. A speckled image, as a fax is (more than 2% of its dark pixels without a dark neighbour, where clean
scans down to 72 dpi have under 0.3%), is cleared of its specks with a 3×3 median filter: tesseract's Latin model
reads specks beside letters as diacritics, and a speckled Danish or Polish fax goes from 12 to 14% of its characters
wrong to under 2%. On an evenly lit page, every pixel lighter than 72% of the way from its ink (darkest half percent)
to its paper (90th percentile) is made white: text showing through from the back of the page would otherwise fill
the gaps between words. They are then resized so their text lines are about 32 pixels high:
enlarged when they are under 20, and, for the alphabets of Europe, reduced when they are over 40, which tesseract
reads as accurately and a third faster (a 300 dpi scan of 11 pt text is reduced to about 70%). A page photographed at an angle is made flat first: found as the largest
bright region with sharp edges against a darker background, and mapped from the quadrilateral its corners span back
to a rectangle. Tesseract's layout analysis loses almost all text from a page skewed by as
little as 2.5°.

Images are read in PNG, JPEG, TIFF (every page), GIF, BMP, PNM, JPEG 2000, WebP, Photoshop (the flattened image),
PCX, TGA and JBIG2. HEIC, HEIF and AVIF images, the
default of many phone cameras, are decoded with libheif: linked into the static Linux executable, and bundled with
tesseract in the other Linux and the Windows release binaries. macOS decodes them with its own `sips` (AVIF from
macOS 13). A build without the bundle uses libheif's `heif-dec` when it is on `PATH` (packaged as `libheif-examples`
or with `libheif`); without it they are reported with an `unsupported` error rather than as images without text.

## Supported formats

The native library contains Tika's standard parser package and these parser modules: Microsoft Office (DOC, DOCX, XLS,
XLSX, PPT, PPTX, MSG), PDF, HTML, Apple iWork, audio/video metadata, CAD, encrypted documents, fonts, mail (EML, MBOX,
PST), legacy office formats (WordPerfect, Lotus, dBase), RSS/Atom, OCR via tesseract, archives (ZIP, TAR, 7Z, RAR),
text, XML, web archives (WARC), source code and SQLite databases (every table's rows, and the documents stored in their
columns). Audio and video files yield their tags (titles, artists, lyrics).
Some formats Tika reads as raw text are read for their content instead: subtitles (SubRip, WebVTT, SubStation Alpha)
as the spoken lines, without timings or markup; Jupyter notebooks cell by cell, code and outputs included;
FictionBook (FB2) books with their chapters, title and authors; and Apple Mail `.emlx` files as the email they hold.
Pages, Numbers and Keynote documents from iWork 2013 on, of which Tika only lists the files, are read for their text,
slides and table cells; Hancom Office HWPX documents, which Tika reads as a plain ZIP archive, for their paragraphs,
tables and footnotes, without running headers and footers; DjVu documents, which Tika detects but cannot read, for
the hidden text layer of each page, or by OCR for a page without one, rendered by dijo's own JB2 and IW44
decoders on every platform, with nothing installed; and Kindle books (MOBI, AZW, AZW3 without DRM), which Tika also only detects, for
their text, title and authors.

Word and OpenDocument text is read as the document stands: tracked insertions are text, tracked deletions are not
(`OfficeParserConfig::set_include_deleted_content` keeps them), and text formatted as hidden is text. Comments follow
the paragraph they are anchored in (OpenDocument) or the document (Word), never cutting the sentence around their
anchor; in Word 97-2003 files, the anchors of comments, footnotes and pictures no longer break a line either.

`just conformance` extracts Apache Tika's own 1094 test documents both natively and with the same Tika
version on the JVM, and compares the text. [`conformance/MATRIX.md`](conformance/MATRIX.md) lists every media type
with how many of its files work, differ or fail: 1069 work or are empty on both sides. Nearly all of the rest differ
only by the file name of a preview image Tika lists (the previews of camera raw photos, iWork and Office documents),
which dijo does not parse, since a preview repeats its document's own front page. A nightly job fails when a file
that worked stops working.

## Development

Every development dependency (Rust toolchain, cargo-nextest, tesseract with exactly the language models the OCR
bundle carries, a Java runtime for running Apache Tika in the comparisons, and just) comes from [devenv](https://devenv.sh). Enter it with `devenv shell`, or run
`direnv allow` once to load it on entry.

All commands are recipes in the `justfile`; run `just` to list them.

| Recipe | What it does |
| --- | --- |
| `just check` | Every gate in order: format check, lint, tests |
| `just build` | Build the workspace |
| `just ocr-bundle [dir]` | Assemble the tesseract library a build loads, its language data and their licences for `DIJO_OCR_BUNDLE` (built with vcpkg, which on NixOS needs a container such as `ubuntu:22.04`) |
| `just static-native` | Build the statically linked Linux executable as released, with tesseract and libheif linked in, into `target/static/dijo` (uses nix) |
| `just container` | Build the container image around `target/static/dijo` and load it into Docker as `dijo:local` |
| `just conformance` | Compare extraction of Tika's own test documents with Tika on the JVM and write conformance/MATRIX.md |
| `just test` | Run the tests with cargo-nextest, then the doc tests |
| `just test-large` | Extract generated inputs of up to 1 GiB with a release build and check peak memory stays under 512 MiB (nightly in CI) |
| `just language-groups` | Retrain the models that tell closely related languages apart, from pinned Wikipedia shards (about 7 GB) |
| `just language-accuracy <tsv>` | Compare language detection with Tika's CharSoup, per language, on a TSV of codes and texts |
| `just quality` | Score extraction against the quality corpus and write quality/SCOREBOARD.md |
| `just quality-bless` | Accept the current quality scores as the new baseline |
| `just quality-corpus` | Regenerate the quality corpus from its ground truth (uses LibreOffice via nix) |
| `just lint` | Clippy over every target, warnings fail |
| `just fmt` | Format every Rust source |
| `just bench` | Run the criterion benchmarks |
| `just licenses` | Regenerate THIRD-PARTY-LICENSES.md from the Rust crates and models |
| `just sbom` | Write the CycloneDX SBOM of the Rust crates to target/sbom |
| `just deps` | cargo-deny's advisory, licence and ban checks, and a check that THIRD-PARTY-LICENSES.md is current |

## License

Apache License 2.0, see [LICENSE](LICENSE). Originally developed by [Yobix AI](https://yobix.ai).

The binary contains third-party Rust crates, C libraries and models; their licences and notices are in
[THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md) and printed by `dijo --licenses`.
