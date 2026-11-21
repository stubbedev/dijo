# dijo

Extract text and metadata from documents. One native binary, no JVM.

- **Every format Apache Tika supports**: PDF, Office (DOC, DOCX, XLS, XLSX, PPT, PPTX, MSG), OpenDocument, HTML, email
  (EML, MBOX, PST), archives (ZIP, TAR, 7Z, RAR), ebooks, images with OCR and the rest of Tika's list. On Tika's own
  1094 test documents, `just conformance` matches Tika's text for 1051 (or both are empty), and most of the others are Tika bugs dijo
  fixes on purpose.
- **Automatic language detection** of the text, for OCR and in the output, covering every national language of Europe:
  86.7% of single sentences right, where Tika's detector gets 80.5%. OCR reads each page with the model of the
  language it detects there.
- **Worn and old print read twice**: a page tesseract reads with a confidence of 90 or less, in Latin script,
  Devanagari, Chinese or Japanese, is read again with [PP-OCRv5](https://github.com/PaddlePaddle/PaddleOCR)'s text
  detector and recogniser for its script, and the more confident reading kept. On the quality corpus this halves the
  character errors of the 1520 *Epigrammata* (16.0% to 4.5%), the 1605 *Quixote* (12.0% to 5.8%), the 1779 *Zoologia
  Danica* (10.3% to 5.7%) and two pages of 1895 Devanagari (33.1% to 14.6%, 52.8% to 21.4%); pages tesseract reads
  confidently cost nothing more. Such a page is first flattened by PaddleOCR's UVDoc and read again by tesseract, which
  undoes the curl of a page photographed in an open book.
- **Faster and lighter**: a 28-page PDF in 55 ms and 23 MiB, against 4.0 s and 301 MiB for `tika-app` 4.1.0; a DOCX in
  6 ms and 17 MiB, against 3.3 s and 213 MiB (`hyperfine` on `test_files/documents/2022_Q3_AAPL.pdf` and
  `ooxml/sorvino.docx`). Files of any size stream through bounded memory.
- **More accurate text**: on the 333 ground-truth documents of `quality/` compared with Tika, Docling and Kreuzberg,
  dijo recalls 98.1% of the words, Tika 76.0%; on its 122 scanned pages, 94.8% against 48.6%
  ([comparison](quality/COMPARISON.md), [how](docs/beyond-tika.md)).

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
  for blackletter (Fraktur), and `script/Latin`, `script/Cyrillic`, `script/Greek` and `script/Armenian`; and the
  PP-OCRv5 models of [`ci/ppocr.tsv`](ci/ppocr.tsv), converted to ONNX (`just ppocr-models` reproduces them) and run
  with [rten](https://github.com/robertknight/rten), a runtime written in Rust.

From source, every build embeds its own OCR: tesseract patched as the release binaries' is, the language data, the
PP-OCRv5 models and, except on macOS, a HEIC/AVIF decoder. It never uses a `tesseract` installed on the system. Assemble that bundle once
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
| tika-server's API | Every route of tika-server 4.2, and what clients of tika-server 3 ask of them, so a Tika client can switch to dijo by its base URL alone. See below. |

### tika-server's API

| Endpoint | Answer |
| --- | --- |
| `GET /` | The routes dijo serves, as HTML or, by `Accept`, plain text. |
| `GET /tika` | The greeting Tika clients check the server is up by. |
| `PUT /tika` | The text, as `POST /extract` answers it. `Accept: text/html` or `text/xml` answers the XHTML, and `application/json` the JSON object of `/tika/xml` below, as tika-server 3 did. |
| `PUT /tika/text`, `/tika/html`, `/tika/xml`, `/tika/md` | The text, the XHTML (as `text/html` or `text/xml`) or Markdown. `Accept: application/json` answers one JSON object, as tika-server 3 did: the metadata in tika-server 3's keys and the content in `X-TIKA:content` as the handler writes it. |
| `PUT /tika/json[/{handler}]` | One JSON object: the metadata under Tika's keys, the content in `tk:content` as the handler (`text`, `body`, `html`, `xml`, `md`, the default, or `ignore`, in any case) writes it, and errors in `tk:exception:container-exception`. 200 even when the document could not be read. |
| `POST /tika/config[/{text,html,xml,md}]` | As `PUT /tika/{text,html,xml,md}`, from a multipart upload (see below); Markdown, or the handler its configuration names, without a format in the path. |
| `POST /tika/config/json[/{handler}]` | As `PUT /tika/json[/{handler}]`, from a multipart upload. |
| `PUT /rmeta[/{handler}]` | A JSON array of such objects: the upload, then every document embedded in it, with `tk:embedded-id`, `tk:embedded-resource-path`, `tk:embedded-depth` and `tk:resource-name`. |
| `POST /rmeta/form[/{handler}]`, `POST /rmeta/config[/{handler}]` | As `PUT /rmeta[/{handler}]`, from a multipart upload. |
| `PUT /meta`, `PUT /meta/{field}` | The metadata alone, as JSON or, by `Accept`, CSV or an XMP packet (`application/rdf+xml`); one field also as plain text, 404 when the document has none. |
| `POST /meta/form`, `POST /meta/config` | As `PUT /meta`, from a multipart upload; `/meta/config` as JSON or CSV alone, as tika-server answers it. |
| `PUT /unpack`, `PUT /unpack/all[/{handler}]` | The embedded documents as a ZIP archive, each named by its `tk:embedded-id` and its type's extension; 204 when there are none. `/all` adds the upload as `0` and each document's metadata as `<name>.metadata.json`, with its content in `tk:content` as a named handler writes it. |
| `POST /unpack`, `POST /unpack/all[/config][/{handler}]` | As `PUT /unpack` and `PUT /unpack/all[/{handler}]`, from a multipart upload. |
| `PUT /detect` (and `/detect/stream`) | The detected media type. |
| `PUT` or `POST /language` (and `/language/stream`, `/language/string`) | The language of the body, which is text, as an ISO 639-1 code where there is one. |
| `GET /version`, `/parsers`, `/parsers/details`, `/detectors`, `/mime-types`, `/mime-types/{type}/{subtype}` | dijo's version, parsers, detectors and media types, as JSON or plain text by `Accept`. |
| `GET /status` | `status`, `active_tasks`, `millis_since_last_parse_started` and `files_processed`, as tika-server answers them when it serves `/status`. |
| `PUT /tika/preset/{name}/…`, `/rmeta/preset/{name}/…`, `/unpack/preset/{name}/…` | 404 `No such preset`, as tika-server answers without presets configured: dijo has none. |
| `POST /pipes`, `POST /async` | 404, as tika-server answers unless configured with fetchers and emitters, which dijo has none of. |

A multipart upload is `multipart/form-data`. The `/form` endpoints read the document from the first part. The
`/config` endpoints read it from the part named `file`, or the first unnamed part, and a parse configuration, as Tika's
JSON, from the part named `config`; other parts are ignored. The file name and media type of the document come from its
part's `Content-Disposition` and `Content-Type`. Of the configuration dijo applies or decides by itself:

| Configuration | |
| --- | --- |
| `basic-content-handler-factory` `type` | Applied: the handler, on the endpoints without one in the path; naming one in both is refused with 400, as tika-server refuses it. Any other setting is refused. |
| `tesseract-ocr-parser` `language` | Applied, as `X-Tika-OCRLanguage`; naming the languages in both is refused with 400. |
| The rest of `tesseract-ocr-parser` (`skipOcr`, `minFileSizeToOcr`, …) and all of `pdf-parser` (`pages.text`, `pages.render.dpi`, `ocrStrategy`, …) | Ignored: dijo OCRs only the images and pages without usable text, at the resolution it picks. |
| `timeout-limits` | Ignored: dijo stops at `--timeout`. |
| Any other component | Refused with 400, rather than extracted as if it had not been asked for. |

tika-server answers its `/config` endpoints only when started with `allowPerRequestConfig`; dijo always does, since none
of the configuration it accepts can do more than these choices.

When an extraction dies, an upload endpoint answers 503 with tika-server's JSON for its forked parser's death, which
clients read the cause from: `{"status":"TIMEOUT"}` when it overran `--timeout`, `{"status":"OOM"}` when its worker
process exceeded `--worker-max-rss-bytes`, and `{"status":"UNSPECIFIED_CRASH"}` when its worker died. A document whose
extraction timed out after some of its text is answered 200 with that text, as tika-server answers a partial timeout.

Every upload endpoint reads the passwords in `Password` headers, a file name in `Content-Disposition` and the tesseract
languages in `X-Tika-OCRLanguage`. dijo decides when and how to OCR and when to stop, so it ignores
`X-Tika-OCRskipOcr`, `X-Tika-PDFOcrStrategy`, `X-Tika-PDFocrDPI`, `X-Tika-OCRminFileSizeToOcr` and
`X-Tika-Timeout-Millis`; `/unpack` does not OCR at all. Any other `X-Tika-` header is refused with 400, and an `Accept`
the endpoint has no form for with 406.

tika-server 4 renamed the metadata keys of tika-server 3: `X-TIKA:content` became `tk:content`, `pdf:charsPerPage`
`pdf:chars-per-page`, `access_permission:can_print` `access-permission:can-print`. dijo derives each 3.x key from its
4.x key by Tika's own 3.x-to-4.x key table and the prefix rules of its `LegacyKeyMigrationFilter`. The endpoints both
serve, `/rmeta`, `/meta`, `/meta/{field}` and the metadata of `/unpack/all`, carry each value under both keys, so a
client of either reads the keys it knows, and `/meta/{field}` answers a field by either name; `/tika/json` has only the
4.x keys, and the JSON object of `/tika/{handler}` only the 3.x keys.

Where the answers differ from tika-server's:

- The metadata is dijo's: Tika's keys, but parsers named `dijo::…`, and none of the keys only Tika's own parsers record.
- `/rmeta` lists the documents in the order they are found, each container before the documents in it; tika-server
  lists a container after them, but the upload first.
- `/unpack` names a document of type `text/plain; charset=…` `.txt`, where tika-server names it `.bin`.
- An empty upload is a document without text, answered 200, where tika-server answers 422.
- Each e-mail message's body, attached messages and mbox entries included, starts with its subject as a heading;
  Tika keeps the subject only in the metadata and the `<title>`, so its batch text shows only the outermost one's.
- Text, JSON, Markdown, pages and elements output leave out the targets (URIs) of a PDF's link annotations, which
  are not text on the page (`--no-normalize links` keeps them); XHTML keeps them, as Tika writes them.
- An OpenDocument spreadsheet writes only the page styles its sheets are printed with, and their header and footer
  fields without the placeholder text LibreOffice stores (`???`, `00/00/0000`); Tika writes every page style.
- `/mime-types/{type}/{subtype}` has no description, acronym or links, which dijo's media types do not carry.
- An XMP packet carries the properties Tika's XMP converters map, from dijo's metadata.
- `GET /` lists dijo's routes, not tika-server's Java resource classes and methods.

Where the answers differ from tika-server 3's:

- `/tika/text`, `/tika/html` and `/tika/xml` answer the content unless `Accept` asks for `application/json`, as
  tika-server 4 does; tika-server 3 answered nothing but its JSON object there.
- The JSON object of `/tika/{handler}` also has `tk:content-handler-type`, as `/tika/json` does.
- `/rmeta` has each document's content twice, in `tk:content` and `X-TIKA:content`, and every other renamed key under
  both names.
- `pdf:charsPerPage` and `pdf:unmappedUnicodeCharsPerPage` count each page's text layer, as Tika 3 and 4 count them,
  not the OCR text dijo adds in the same pass, and `pdf:ocrPageCount` counts the pages Tika would OCR, as Tika 4.1 counts them, including those dijo OCRs itself.
  dijo OCRs the pages without usable text in the first request, so a second request for an OCR pass answers the same.

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
