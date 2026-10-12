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

Building compiles Tika, so the build machine needs a GraalVM JDK with `native-image`, pointed to by `GRAALVM_HOME`,
matching the version `extractous-core/tika-native/build.gradle` requires. The first build takes a few minutes; later
builds reuse the compiled library until the Tika sources or the GraalVM release change.

The compiled library and the JDK libraries GraalVM emits beside it (`libawt.so`, `libfontmanager.so`, ...) are
embedded in the binary, compressed (about 36 MB). On first use they are unpacked into
`$XDG_CACHE_HOME/extractous/<fingerprint>/` (`~/.cache` on Linux, `~/Library/Caches` on macOS, `%LOCALAPPDATA%` on
Windows) and loaded from there, so the binary runs wherever it is copied.

OCR runs the `tesseract` executable, which must be on `PATH` together with the language data you request.

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
| `--no-normalize <STEP,...>` | Leave out normalisation steps: `invisible`, `ligatures`, `soft-hyphens`, `unicode`, `whitespace` |
| `--raw` | No normalisation at all: the text exactly as Tika extracted it |
| `--password <PW>`, `--password-file <FILE>` | Passwords tried, in order, on encrypted documents (the file or `EXTRACTOUS_PASSWORD` keeps them out of process listings) |
| `--timeout <SECONDS>` | Per-input limit; the text extracted so far is kept and the input reported as partial |
| `--embedded inline\|separate` | Add embedded documents' text to their container's (default), or extract each as its own document, recursively |
| `--max-depth`, `--max-embedded`, `--max-expanded-bytes` | Limits for `--embedded separate`: nesting depth, number of embedded documents, and bytes they may expand to |
| `--cache-dir <DIR>`, `--cache-max-size <BYTES>` | Reuse the output of complete extractions while the input, the options and the extractous build are unchanged; evict least recently used entries beyond the size |
| `--spool-dir <DIR>` | Where temporary files go: spooled input and output, embedded documents, Tika's PDF scratch files |
| `--strict` | Fail an input when any part of it could not be extracted, not only when nothing could |
| `-m, --metadata` | Include metadata: on standard error for text and xhtml, inline for json |
| `-o, --output <DIR>` | Write one file per input into DIR, mirroring the input paths |
| `-r, --recursive` | Descend into directories, in sorted order |
| `-j, --jobs <N>` | Inputs extracted in parallel (default: number of cores); output stays in input order |
| `--ocr auto\|never\|always` | OCR pages and images without text (default), never, or every PDF page as well |
| `--ocr-lang <LANGS>` | Tesseract languages, such as `deu` or `eng+ara` |

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
appears in a row. Tabs are kept, since they separate table cells. Normalisation streams, so it holds back at most a few
characters. XHTML output is never normalised.

With `--cache-dir`, a run over files that have not changed only hashes them and copies their stored output. An entry is
keyed by the BLAKE3 hash of the input's content and its path, the options that shape the output, the extractous
executable itself (so a new build, Tika version or embedded library invalidates everything) and, unless OCR is off,
the tesseract version. Only complete extractions are stored, each written to a temporary file and renamed into place,
so concurrent runs and crashes never leave a broken entry. The cache is not available with passwords, so decrypted text
never lands on disk, or with `--embedded separate`.

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
come back as 200 with their status in the JSON. `--max-input-bytes` caps uploads (413) and `--timeout` bounds a whole
request (408 while uploading, 504 before extraction starts, a cut connection after). SIGTERM stops accepting and waits
`--shutdown-grace` seconds for running requests.

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

`metadata` (with `-m`) holds normalised fields filled from whichever Tika key a format uses: dates are RFC 3339 in UTC,
or marked `unknown_offset` when the document gave no offset; multi-valued fields are always arrays. Tika's own keys are
available with `--raw-metadata`. `status` is `complete`, `partial` (some text before an error), `encrypted` (no password opened it) or `failed`, and
every error carries a category and its full cause chain. Decrypted documents report `"encrypted":true,"decrypted":true`
in `metadata`; passwords never appear in output or errors. Passwords decrypt PDF (a PDF with only an owner password
needs none), DOCX/XLSX/PPTX, Word, Excel and PowerPoint 97-2003, Access and 7z. Tika cannot decrypt ODF documents or
encrypted ZIP entries; those are reported as `encrypted`, and an encrypted entry leaves the rest of its archive
extracted with status `partial`.

### Embedded documents

With `--embedded separate` (or `embedded=separate` on the server) every document inside an input (archive members,
email attachments, objects embedded in Office files, images in documents) is extracted as a document of its own,
depth first, right after its container. Each record names the input in `source.path` and the document's place inside
it in `source.embedded_path` (`mail.eml/numbers.xlsx/image1.png`), with its size and BLAKE3 hash. A document whose
content already appeared in the same input is reported once more only as `"duplicate_of"` its first occurrence. In
text output each embedded document starts with a `=== path ===` line. Exceeding a limit skips that subtree with a
`limit-exceeded` error instead of failing the input.

## Supported formats

The native library contains Tika's standard parser package and these parser modules: Microsoft Office (DOC, DOCX, XLS,
XLSX, PPT, PPTX, MSG), PDF, HTML, Apple iWork, audio/video metadata, CAD, encrypted documents, fonts, mail (EML, MBOX,
PST), legacy office formats (WordPerfect, Lotus, dBase), RSS/Atom, OCR via tesseract, archives (ZIP, TAR, 7Z, RAR),
text, XML, web archives (WARC), source code and advanced media.

Compiled in is not the same as working: native-image needs reflection metadata for each parser it reaches. The test
suite covers PDF, DOC, DOCX, PPTX, XLSX, ODT, CSV, EPUB, and PNG and PDF through OCR.

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
