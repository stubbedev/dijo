# dijo

Extract text and metadata from documents. One native binary, no JVM.

- **Every format Apache Tika supports**: PDF, Office (DOC, DOCX, XLS, XLSX, PPT, PPTX, MSG), OpenDocument, HTML, email
  (EML, MBOX, PST), archives (ZIP, TAR, 7Z, RAR), ebooks, images with OCR and the rest of Tika's list. On Tika's own
  1094 test documents, `just conformance` matches Tika's text for 1051 (or both are empty), and most of the others are Tika bugs dijo
  fixes on purpose.
- **Automatic language detection** of the text, for OCR and in the output, covering every national language of Europe:
  86.7% of single sentences right, where Tika's detector gets 80.5%. OCR reads each page with the model of the
  language it detects there.
- **Faster and lighter**: a 28-page PDF in 55 ms and 23 MiB, against 4.0 s and 301 MiB for `tika-app` 4.1.0; a DOCX in
  6 ms and 17 MiB, against 3.3 s and 213 MiB (`hyperfine` on `test_files/documents/2022_Q3_AAPL.pdf` and
  `ooxml/sorvino.docx`). Files of any size stream through bounded memory.
- **More accurate text**: on the 240 ground-truth documents of `quality/` compared with Tika, dijo recalls 99.6% of
  the words, Tika 84.8% ([comparison](quality/COMPARISON.md)).

## Install

Each [release](https://github.com/stubbedev/dijo/releases) has archives for Linux (x86_64, aarch64), macOS (aarch64)
and Windows (x86_64), plus `SHA256SUMS`, an SBOM and build provenance
(`gh attestation verify <archive> --repo stubbedev/dijo`).

- The `x86_64-linux-static` and `aarch64-linux-static` binaries need nothing installed, not even a C library, and run
  in a `FROM scratch` container. OCR and HEIC/AVIF decoding are built in.
- The other release binaries need the system C library (glibc 2.35 or later on Linux) and carry their own OCR library
  and language data.
- Every release binary carries the tesseract models of [`ci/tessdata.tsv`](ci/tessdata.tsv): one for every national
  language of Europe (`bel bos bul cat ces cym dan deu ell eng est eus fao fin fra gle glg hrv hun hye isl ita kat lav
  lit ltz mkd mlt nld nor pol por ron rus slk slv spa sqi srp srp_latn swe tur ukr`), one for each other script
  detection finds (`amh ara ben bod chi_sim guj heb hin jpn kan khm kor lao mal mya pan sin tam tel tha`), `deu_latf`
  for blackletter (Fraktur), and `script/Latin`, `script/Cyrillic`, `script/Greek` and `script/Armenian`.

From source, every build embeds its own OCR: tesseract patched as the release binaries' is, the language data and,
except on macOS, a HEIC/AVIF decoder. It never uses a `tesseract` installed on the system. Assemble that bundle once
(vcpkg builds it) and name it when building:

```sh
just ocr-bundle target/ocr-bundle
DIJO_OCR_BUNDLE=$PWD/target/ocr-bundle cargo install --locked --git https://github.com/stubbedev/dijo dijo
```

The Nix development shell (`nix develop`) sets `DIJO_OCR_BUNDLE` to a bundle it builds itself.

## Command line

```sh
dijo report.pdf                         # plain text to standard output
dijo -f xhtml report.docx               # XHTML
dijo -f json -m *.pdf                   # one JSON object per file, with metadata
dijo -r -o out/ archive/                # one output file per input, mirroring archive/
cat scan.tiff | dijo --ocr-lang deu -   # standard input, OCR in German
dijo -f pages manual.pdf                # one JSON line per page
dijo detect mystery.bin                 # application/pdf
```

| Option | Meaning |
| --- | --- |
| `-f, --format text\|xhtml\|markdown\|json\|pages\|elements` | Plain text (default), XHTML, Markdown, one JSON object per input, or one JSON line per page or per element |
| `-m, --metadata` | Include metadata: on standard error for text and xhtml, inline for json |
| `--raw-metadata` | Add the raw, format-specific metadata keys to json output |
| `-o, --output <DIR>` | Write one file per input into DIR, mirroring the input paths |
| `-r, --recursive` | Descend into directories, in sorted order |
| `-j, --jobs <N>` | Inputs extracted in parallel (default: the number of cores); output stays in input order |
| `--ocr auto\|never\|always` | OCR images and PDF pages without a usable text layer (default); never; or every PDF page |
| `--ocr-lang <LANGS>` | Tesseract languages, such as `deu` or `eng+ara`; by default the script, then the language, is detected per image. A model that is not installed is a usage error listing those that are |
| `--password <PW>`, `--password-file <FILE>` | Passwords tried, in order, on encrypted documents |
| `--embedded inline\|separate` | Add embedded documents' text to their container's (default), or extract each as its own document |
| `--max-depth`, `--max-embedded`, `--max-expanded-bytes` | Limits for `--embedded separate` |
| `--timeout <SECONDS>` | Per-input limit; text extracted so far is kept and the input reported as partial |
| `--max-heap-bytes <BYTES>` | Cap the memory one extraction may allocate |
| `--strict` | Fail an input when any part of it could not be extracted |
| `--raw`, `--no-normalize <STEP,...>` | Turn off text normalisation entirely, or only some steps (`invisible`, `ligatures`, `soft-hyphens`, `unicode`, `whitespace`, `headers`, `hyphenation`, `reading-order`, `bookmarks`) |
| `--cache-dir <DIR>`, `--cache-max-size <BYTES>` | Reuse the output of unchanged inputs |
| `--spool-dir <DIR>` | Where temporary files go |

Every option also reads from an environment variable (`DIJO_FORMAT`, `DIJO_JOBS`, `DIJO_PASSWORD`, ...); `dijo --help`
lists them all.

The exit status is 0 when text was extracted from every input, 1 when nothing could be extracted from at least one
(with `--strict`, when any part of one failed) and 2 on a usage error. One failed input does not stop the others, and
damage inside a document (a broken attachment or page) does not discard the rest of it.

Text output is normalised for indexing: invisible characters and soft hyphens dropped, ligatures expanded, Unicode
NFC, whitespace collapsed, words hyphenated across lines joined, PDF pages put in reading order. In `--format pages`,
running headers and footers move into each page record's `header` and `footer`.

## Server

```sh
dijo serve --listen 0.0.0.0:9998
curl --data-binary @report.pdf -H 'X-Dijo-Filename: report.pdf' localhost:9998/extract
curl --data-binary @report.pdf 'localhost:9998/extract?format=json&metadata=true'
curl --data-binary @mystery.bin localhost:9998/detect
```

| Endpoint | |
| --- | --- |
| `POST /extract` | The body is the document. Query: `format`, `metadata`, `raw_metadata`, `raw`, `no_normalize`, `ocr`, `ocr_lang`, `embedded`, as on the command line. Passwords go in `X-Dijo-Password` headers, a file name hint in `X-Dijo-Filename`. |
| `POST /detect` | The detected media type, as text. |
| `GET /healthz`, `GET /readyz` | Liveness, and whether a request would be accepted now. |
| `GET /metrics` | Prometheus metrics. |
| tika-server's API | The endpoints of tika-server 4.1, so a Tika client can switch to dijo by its base URL alone. See below. |

### tika-server's API

| Endpoint | Answer |
| --- | --- |
| `GET /tika` | The greeting Tika clients check the server is up by. |
| `PUT /tika` | The text, as `POST /extract` answers it. `Accept: text/html` or `text/xml` answers the XHTML, as tika-server 3 did. |
| `PUT /tika/text`, `/tika/html`, `/tika/xml`, `/tika/md` | The text, the XHTML (as `text/html` or `text/xml`) or Markdown. |
| `PUT /tika/json[/{handler}]` | One JSON object: the metadata under Tika's keys, the content in `tk:content` as the handler (`text`, `html`, `xml`, `md`, the default, or `ignore`) writes it, and errors in `tk:exception:container-exception`. 200 even when the document could not be read. |
| `PUT /rmeta[/{handler}]` | A JSON array of such objects: the upload, then every document embedded in it, with `tk:embedded-id`, `tk:embedded-resource-path`, `tk:embedded-depth` and `tk:resource-name`. |
| `PUT /meta`, `PUT /meta/{field}` | The metadata alone, as JSON or, by `Accept`, CSV; one field also as plain text, 404 when the document has none. |
| `PUT /unpack`, `PUT /unpack/all` | The embedded documents as a ZIP archive, each named by its `tk:embedded-id` and its type's extension; 204 when there are none. `/all` adds the upload as `0` and each document's metadata as `<name>.metadata.json`. |
| `PUT /detect` (and `/detect/stream`) | The detected media type. |
| `PUT` or `POST /language` (and `/language/stream`, `/language/string`) | The language of the body, which is text, as an ISO 639-1 code where there is one. |
| `GET /version`, `/parsers`, `/parsers/details`, `/detectors`, `/mime-types`, `/mime-types/{type}/{subtype}` | dijo's version, parsers, detectors and media types, as JSON or plain text by `Accept`. |

Every upload endpoint reads the passwords in `Password` headers, a file name in `Content-Disposition` and the tesseract
languages in `X-Tika-OCRLanguage`. dijo decides when and how to OCR and when to stop, so it ignores
`X-Tika-OCRskipOcr`, `X-Tika-PDFOcrStrategy`, `X-Tika-PDFocrDPI`, `X-Tika-OCRminFileSizeToOcr` and
`X-Tika-Timeout-Millis`; `/unpack` does not OCR at all. Any other `X-Tika-` header is refused with 400, and an `Accept`
the endpoint has no form for with 406.

Where the answers differ from tika-server's:

- The metadata is dijo's: Tika's keys, but parsers named `dijo::…`, and none of the keys only Tika's own parsers record.
- `/rmeta` lists the documents in the order they are found, each container before the documents in it; tika-server
  lists a container after them, but the upload first.
- `/unpack` names a document of type `text/plain; charset=…` `.txt`, where tika-server names it `.bin`.
- An empty upload is a document without text, answered 200, where tika-server answers 422.
- `/mime-types/{type}/{subtype}` has no description, acronym or links, which dijo's media types do not carry.
- The presets, the `/config` and `/form` uploads, the XMP endpoints, `/translate`, `/pipes` and `/async` are not served.

Responses stream as the document is extracted. A document that cannot be extracted gets a JSON error with 415
(unsupported), 422 (corrupt or encrypted) or 500; partial results come back as 200 with their status in the JSON.
When every worker and queue slot is busy, requests get 503 with `Retry-After`.

Useful options: `--workers`, `--queue`, `--timeout`, `--max-input-bytes`, `--max-heap-bytes`, `--max-output-chars`,
`--cache-dir`, and `--isolation process`, which runs each worker in its own process so a crash or runaway document
takes down one request rather than the server (`--worker-max-rss-bytes` caps each worker's memory on Linux).
`dijo serve --help` lists them all.

The server parses untrusted input defensively (no external XML entities, capped entity and archive expansion, bounded
queues and timeouts), but has no authentication or TLS: put it behind a reverse proxy or inside a private network.

### Container

```sh
docker run --rm -p 9998:9998 ghcr.io/stubbedev/dijo:latest
docker run --rm -v "$PWD:/data:ro" ghcr.io/stubbedev/dijo:latest /data/report.pdf
```

The image (linux/amd64, linux/arm64) is the static binary `FROM scratch`, running the server as an unprivileged user.
It declares a healthcheck, so `docker ps` and Compose report the server's health with no configuration: `dijo health`
asks the server at `DIJO_LISTEN` for `/healthz` and exits 0 when it answers 200, or 1 with the reason on standard error.
Set the address through `DIJO_LISTEN` rather than `--listen` so that the check finds it. `--ready` checks `/readyz`
instead, which fails while every worker and queue slot is busy, and `--timeout` (5 seconds by default) bounds the wait.

```yaml
services:
  dijo:
    image: ghcr.io/stubbedev/dijo:latest
    environment:
      DIJO_LISTEN: 0.0.0.0:8080
  indexer:
    depends_on:
      dijo:
        condition: service_healthy
```

## JSON output

`--format json` writes one object per input per line, following
[`schema/output-v1.schema.json`](schema/output-v1.schema.json):

```json
{"schema_version":1,"kind":"document",
 "source":{"path":"report.pdf","size":266240,"blake3":"0b9b01a7..."},
 "text":"...",
 "mime_type":"application/pdf",
 "status":"complete",
 "errors":[],
 "detected_language":{"code":"eng","script":"Latin","confidence":0.996,"reliable":true},
 "metadata":{"title":"...","authors":["..."],"created":{"utc":"2022-07-29T10:03:21Z"},"page_count":28}}
```

- `status` is `complete`, `partial` (some text before an error), `encrypted` (no password opened it) or `failed`;
  every error carries a category and its cause chain.
- `metadata` (with `-m`) has normalised fields: RFC 3339 dates in UTC, multi-valued fields as arrays.
- `detected_language` is detected from the text itself, separate from the language the document declares.
- `--format pages` and `--format elements` write a record per page or per heading, paragraph, list item and table as
  soon as each is complete, followed by a `"kind":"document"` record with the status and metadata.
- With `--embedded separate`, each embedded document (attachment, archive member, embedded object) is a record of its
  own, with its place in the input in `source.embedded_path`.

## Development

Dependencies come from the flake's dev shell: `nix develop`, or `direnv allow` once. Commands are recipes in the
`justfile`; `just` lists them.

| Recipe | What it does |
| --- | --- |
| `just check` | Format check, lint and tests |
| `just test` | Unit, integration and doc tests |
| `just test-large` | Generated inputs up to 1 GiB, checking peak memory stays bounded |
| `just quality` | Score extraction against the ground-truth corpus in `quality/` |
| `just conformance` | Compare extraction of Tika's own test documents with Tika on the JVM |
| `just static-native` | Build the static Linux binary into `target/static/dijo` (uses nix) |
| `just container` | Build the container image as `dijo:local` |

## License

Apache License 2.0, see [LICENSE](LICENSE). Third-party licences and notices are in
[THIRD-PARTY-LICENSES.md](THIRD-PARTY-LICENSES.md) and printed by `dijo --licenses`.

dijo began as a fork of [extractous](https://github.com/yobix-ai/extractous) by Yobix AI, and many of its parsers are
ports of [Apache Tika](https://tika.apache.org/), [Apache POI](https://poi.apache.org/) and
[Apache PDFBox](https://pdfbox.apache.org/). Thanks to all of them.
