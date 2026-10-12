# Extractous

Extract text and metadata from documents on the command line, without a JVM.

This is a fork of [yobix-ai/extractous](https://github.com/yobix-ai/extractous). Every format is parsed by
[Apache Tika](https://tika.apache.org/), compiled ahead of time with [GraalVM](https://www.graalvm.org/) native-image
into a shared library that the `extractous` binary calls through JNI. No Java runtime is installed or started; Tika
runs as native code inside the process.

## Install

Each [release](https://github.com/stubbedev/extractous/releases) has archives for Linux (x86_64, aarch64), macOS
(aarch64) and Windows (x86_64) with the binary, its licence and the third-party notices, plus `SHA256SUMS`, CycloneDX
SBOMs of the Rust crates and Java libraries, and build provenance you can check with
`gh attestation verify <archive> --repo stubbedev/extractous`.

To build from source instead:

```sh
cargo install --locked --git https://github.com/stubbedev/extractous extractous
```

Building compiles Tika, so the build machine needs the GraalVM release pinned in `ci/graalvm.env`, pointed to by
`GRAALVM_HOME`; `ci/install-graalvm.sh <platform> <dir>` downloads it, checks its SHA-256 and prints its home. Older
GraalVM releases do not emit the AWT libraries on macOS. The first build takes a few minutes; later
builds reuse the compiled library until the Tika sources or the GraalVM release change.

The compiled library and the JDK libraries GraalVM emits beside it (`libawt.so`, `libfontmanager.so`, ...) are
embedded in the binary, compressed (about 36 MB). On first use they are unpacked into
`$XDG_CACHE_HOME/extractous/<fingerprint>/` (`~/.cache` on Linux, `~/Library/Caches` on macOS, `%LOCALAPPDATA%` on
Windows) and loaded from there, so the binary runs wherever it is copied.

OCR runs the `tesseract` executable, which must be on `PATH` together with its language data. Without `--ocr-lang`,
each image is first run through tesseract's orientation and script detection (the `osd` data, which distribution
packages install with tesseract): it is turned upright and read with the installed model for its script, such as
`script/Arabic`, then `ara`, falling back to `eng`.

Under `--ocr auto`, each PDF page's text layer is checked before it is used. A page with next to no text is OCRed
as a scan. A page whose text layer is garbage is OCRed and its text replaced. Garbage here means private-use or
control characters, symbols where letters should be, words mixing alphabets, too few vowels for a language written
in Latin script, or mostly unmapped glyphs (broken `ToUnicode` maps, custom font encodings). A page that is only
doubtful is OCRed as well and keeps whichever text reads better. The JSON metadata lists each replaced page and the
reason under `rejected_text_layers`. Clean pages are never OCRed.

## Usage

```sh
extractous report.pdf                         # plain text to standard output
extractous -f xhtml report.docx               # Tika's XHTML
extractous -f json -m *.pdf                   # one JSON object per file, with metadata
extractous -r -o out/ archive/                # one output file per input, mirroring archive/
cat scan.tiff | extractous --ocr-lang deu -   # standard input, OCR in German
extractous -f pages manual.pdf                # one JSON line per page, for search indexing
extractous detect mystery.bin                 # application/pdf
```

| Option | Meaning |
| --- | --- |
| `-f, --format text\|xhtml\|json\|pages\|elements` | Plain text (default), Tika XHTML, one JSON object per input, or one JSON line per page or per element |
| `--raw-metadata` | Add Tika's own metadata keys to json output |
| `--no-normalize <STEP,...>` | Leave out normalisation steps: `invisible`, `ligatures`, `soft-hyphens`, `unicode`, `whitespace`, `headers`, `hyphenation` |
| `--raw` | No normalisation at all: the text exactly as Tika extracted it |
| `--password <PW>`, `--password-file <FILE>` | Passwords tried, in order, on encrypted documents (the file or `EXTRACTOUS_PASSWORD` keeps them out of process listings) |
| `--timeout <SECONDS>` | Per-input limit; the text extracted so far is kept and the input reported as partial |
| `--embedded inline\|separate` | Add embedded documents' text to their container's (default), or extract each as its own document, recursively |
| `--max-depth`, `--max-embedded`, `--max-expanded-bytes` | Limits for `--embedded separate`: nesting depth, number of embedded documents, and bytes they may expand to |
| `--cache-dir <DIR>`, `--cache-max-size <BYTES>` | Reuse the output of complete extractions while the input, the options and the extractous build are unchanged; evict least recently used entries beyond the size (also for `serve`) |
| `--cache-partial` | Also cache partial results (never those cut short by a timeout) |
| `--cache-trust-mtime` | Skip rehashing inputs whose path, size and modification time match the last run |
| `--max-heap-bytes <BYTES>` | Cap Tika's Java heap; an extraction that needs more fails with `limit-exceeded` and the next one runs normally |
| `--spool-dir <DIR>` | Where temporary files go: spooled input and output, embedded documents, Tika's PDF scratch files |
| `--strict` | Fail an input when any part of it could not be extracted, not only when nothing could |
| `-m, --metadata` | Include metadata: on standard error for text and xhtml, inline for json |
| `-o, --output <DIR>` | Write one file per input into DIR, mirroring the input paths |
| `-r, --recursive` | Descend into directories, in sorted order |
| `-j, --jobs <N>` | Inputs extracted in parallel (default: number of cores); output stays in input order |
| `--ocr auto\|never\|always` | OCR images, and PDF pages without text or with a garbage text layer (default); never; or every PDF page as well |
| `--ocr-lang <LANGS>` | Tesseract languages, such as `deu` or `eng+ara`; detected per image from its script by default |

Every option has an environment variable equivalent (`EXTRACTOUS_FORMAT`, `EXTRACTOUS_JOBS`, ...); `extractous --help`
lists them. Errors go to standard error with the input path and the full Java cause chain, and one failed input does
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
`header` and `footer`; they are learned from the first 8 pages, which are held back meanwhile. Text and json output
drop them. A word hyphenated
across a line break (`extrac-` / `tion`) is joined. The hyphen stays only when the text before shows a hyphenated
compound: the word already holds a hyphen (`state-of-the-` / `art`), or `wellknown` did not occur but `well-known` or
both `well` and `known` did. With no evidence either way the word is joined, so a compound that breaks at its hyphen
before either half has occurred loses the hyphen. A hyphen before a capital or a digit, or before a blank line,
stays where it is. Normalisation streams, so it holds
back at most a few characters, or a word after a hyphen that ends a line. XHTML output is never normalised.

With `--cache-dir`, a run over files that have not changed only hashes them and copies their stored output. An entry is
keyed by the BLAKE3 hash of the input's content and its name, the options that shape the output, the extractous
executable itself (so a new build, Tika version or embedded library invalidates everything) and the tesseract version.
Only complete extractions are stored, each written to a temporary file and renamed into place, so concurrent runs and
crashes never leave a broken entry; once the cache grows past `--cache-max-size`, the least recently used entries are
evicted. With `--embedded separate`, every embedded document without embedded documents of its own is an entry of its
own, keyed by its own content and file name and replayed with the provenance of wherever it turns up, so the same
attachment in many emails is extracted once; the emails and archives around it are read again, which is cheap. The
cache is not available with passwords, so decrypted text never lands on disk. `extractous serve` takes the same
options: a repeated upload is answered from the cache without starting Tika, requests with passwords bypass it, and
`/metrics` counts hits and misses.

## Server

```sh
extractous serve --listen 0.0.0.0:9998 --workers 8
curl --data-binary @report.pdf -H 'X-Extractous-Filename: report.pdf' localhost:9998/extract
curl --data-binary @report.pdf 'localhost:9998/extract?format=json&metadata=true'
curl --data-binary @mystery.bin localhost:9998/detect
```

| Endpoint | |
| --- | --- |
| `POST /extract` | The request body is the document. Query: `format` (`text`, `xhtml`, `json`, `pages`, `elements`), `metadata`, `raw_metadata`, `raw`, `no_normalize` (comma-separated steps), `ocr`, `ocr_lang`, as on the command line. Passwords go in `X-Extractous-Password` headers, an optional file name hint in `X-Extractous-Filename`. |
| `POST /detect` | The detected media type, as text. |
| `GET /healthz`, `GET /readyz` | Liveness, and whether a request would be accepted now. |
| `GET /metrics` | Prometheus metrics: requests by endpoint and status, failures by category, extraction time per media type, bytes in and out, workers and requests in flight. |

Extraction runs on a fixed pool of worker threads (`--workers`). Uploads are spooled to disk and the response streams as
it is extracted.
When every worker and queue slot (`--queue`) is busy, requests get 503 with `Retry-After`. A document that cannot be
extracted at all gets a JSON error document with 415 (unsupported), 422 (corrupt or encrypted) or 500; partial results
come back as 200 with their status in the JSON. `--max-heap-bytes` caps the Java heap all workers share (an extraction beyond it gets 413), `--max-input-bytes` caps uploads (413) and `--timeout` bounds a whole
request (408 while uploading, 504 before extraction starts, a cut connection after). SIGTERM stops accepting and waits
`--shutdown-grace` seconds for running requests.

`--isolation process` runs each worker as a process of its own instead of a thread, so a parser that crashes, hangs
or runs away with memory takes down one request and not the server. The server restarts the worker for the next
request. A crashed worker fails its request with 500. A worker still running five seconds past `--timeout` is killed
(504). On Linux, `--worker-max-rss-bytes` kills a worker whose resident memory grows past the limit (413). Each
worker process holds its own Java heap, so memory use grows with `--workers`. With 4 workers on a mixed office and PDF
corpus, process isolation took 565 MiB at peak against 163 MiB for threads. It served 243 requests/s against 202 at
4 concurrent requests, and 37 against 52 one at a time.

### Security

Documents and requests are untrusted input. What the server defends against, and how:

- **XML attacks.** No parser resolves external entities or loads external DTDs (XXE), and entity expansion is
  capped, so a billion-laughs document fails as `limit-exceeded`. Tests cover XML, SVG and DOCX.
- **Resource exhaustion.** `--max-input-bytes` caps uploads, `--timeout` caps a request's time, `--max-heap-bytes`
  caps the Java heap, and `--worker-max-rss-bytes` caps a worker process's memory. Embedded documents have depth,
  count and expanded-size limits with `embedded=separate`; inline, Tika's zip-bomb guard applies.
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

What it leaves to the deployment:

- **Access control and transport.** There is no authentication and no TLS: put the server behind a reverse proxy or
  inside a private network.
- **Abuse limits.** There are no per-client rate limits, and no cap on idle connections beyond `--client-timeout`.
- **In-process parsing.** With the default `--isolation thread`, a crash in native code inside a parser ends the
  whole server.
- **Error detail.** Error responses carry the cause chain of the failure, Java exception messages included, but no
  stack traces.

### Container

Each release publishes a multi-platform image (linux/amd64, linux/arm64) with tesseract and English OCR data, running
the server as an unprivileged user:

```sh
docker run --rm -p 9998:9998 ghcr.io/stubbedev/extractous:latest
docker run --rm -v "$PWD:/data:ro" ghcr.io/stubbedev/extractous:latest /data/report.pdf
```

`docker build .` builds the same image from source. Install more `tesseract-ocr-*` language packages in a derived image
for OCR in other languages.

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
declares.

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
HTML and EPUB, a two-column PDF, a report with running headers and footers, Arabic, Hebrew, Chinese, Japanese, Thai,
Hindi and German documents, tables, a PDF hyphenated by TeX, PDFs with broken text layers, and scans at 300 and 150
dpi, skewed and rotated. Every extraction is scored
against its truth: character and token error rate, recall (the share of the truth's words that were extracted at
all), spurious text and paragraph order. The test suite fails when a score gets worse than `quality/baseline.json`
allows, so CI holds every change to it. [`quality/SCOREBOARD.md`](quality/SCOREBOARD.md) has the current scores;
`just quality` refreshes it, `just quality-bless` accepts new scores and `just quality-corpus` regenerates the corpus.

What it shows today: born-digital Office and OpenDocument files and scans (clean, skewed, rotated, or photographed
upside down with an EXIF orientation) extract exactly; PDFs add their bookmark text at the end; right-to-left Arabic
PDFs come out in the wrong word order.

Before OCR, images and rendered PDF pages are turned upright: by their EXIF orientation, and by the skew of their text
lines, estimated from projection profiles. Tesseract's layout analysis loses almost all text from a page skewed by as
little as 2.5°.

## Supported formats

The native library contains Tika's standard parser package and these parser modules: Microsoft Office (DOC, DOCX, XLS,
XLSX, PPT, PPTX, MSG), PDF, HTML, Apple iWork, audio/video metadata, CAD, encrypted documents, fonts, mail (EML, MBOX,
PST), legacy office formats (WordPerfect, Lotus, dBase), RSS/Atom, OCR via tesseract, archives (ZIP, TAR, 7Z, RAR),
text, XML, web archives (WARC) and source code. Audio and video files yield their tags (titles, artists, lyrics).

Compiled in is not the same as working: native-image needs reflection metadata for each parser it reaches.
`just conformance` therefore extracts Apache Tika's own 966 test documents both natively and with the same Tika
version on the JVM, and compares the text. [`conformance/MATRIX.md`](conformance/MATRIX.md) lists every media type
with how many of its files work, differ or fail: 943 work or are empty on both sides. Most of the rest are DOCX files,
where the streaming extractor used for bounded memory leaves out comment bylines and footnote markers. A nightly job
fails when a file that worked stops working.

## Development

Every development dependency (Rust toolchain, cargo-nextest, GraalVM CE, tesseract with the English, German and Arabic
language data the OCR tests use, and just) comes from [devenv](https://devenv.sh). Enter it with `devenv shell`, or run
`direnv allow` once to load it on entry.

All commands are recipes in the `justfile`; run `just` to list them.

| Recipe | What it does |
| --- | --- |
| `just check` | Every gate in order: format check, lint, tests |
| `just build` | Build the workspace, compiling Tika on first run |
| `just test` | Run the tests with cargo-nextest, then the doc tests |
| `just test-large` | Extract generated inputs of up to 1 GiB with a release build and check peak memory stays under 512 MiB (nightly in CI) |
| `just quality` | Score extraction against the quality corpus and write quality/SCOREBOARD.md |
| `just quality-bless` | Accept the current quality scores as the new baseline |
| `just quality-corpus` | Regenerate the quality corpus from its ground truth (uses LibreOffice via nix) |
| `just lint` | Clippy over every target, warnings fail |
| `just fmt` | Format every Rust source |
| `just bench` | Run the criterion benchmarks |
| `just gradle-verification` | Record the SHA-256 of every Gradle dependency after changing build.gradle |
| `just tika-agent` | Run Tika under the GraalVM tracing agent to regenerate native-image metadata |
| `just licenses` | Regenerate THIRD-PARTY-LICENSES.md from the Rust crates and Java libraries |
| `just sbom` | Write CycloneDX SBOMs of the Rust crates and Java libraries to target/sbom |
| `just deps` | cargo-deny's advisory, licence and ban checks, the Java licence allow-list, and a check that THIRD-PARTY-LICENSES.md is current |

## License

Apache License 2.0, see [LICENSE](LICENSE). Originally developed by [Yobix AI](https://yobix.ai).

The binary contains third-party Rust crates and Java libraries; their licences and notices are in
[THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md) and printed by `extractous --licenses`.
