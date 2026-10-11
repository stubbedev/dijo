# Extractous

Extract text and metadata from documents on the command line, without a JVM.

This is a fork of [yobix-ai/extractous](https://github.com/yobix-ai/extractous). Every format is parsed by
[Apache Tika](https://tika.apache.org/), compiled ahead of time with [GraalVM](https://www.graalvm.org/) native-image
into a shared library that the `extractous` binary calls through JNI. No Java runtime is installed or started; Tika
runs as native code inside the process.

## Install

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
extractous detect mystery.bin                 # application/pdf
```

| Option | Meaning |
| --- | --- |
| `-f, --format text\|xhtml\|json` | Plain text (default), Tika XHTML, or one JSON object per input per line |
| `--raw-metadata` | Add Tika's own metadata keys to json output |
| `-m, --metadata` | Include metadata: on standard error for text and xhtml, inline for json |
| `-o, --output <DIR>` | Write one file per input into DIR, mirroring the input paths |
| `-r, --recursive` | Descend into directories, in sorted order |
| `-j, --jobs <N>` | Inputs extracted in parallel (default: number of cores); output stays in input order |
| `--ocr auto\|never\|always` | OCR pages and images without text (default), never, or every PDF page as well |
| `--ocr-lang <LANGS>` | Tesseract languages, such as `deu` or `eng+ara` |

Every option has an environment variable equivalent (`EXTRACTOUS_FORMAT`, `EXTRACTOUS_JOBS`, ...); `extractous --help`
lists them. Errors go to standard error with the input path and the full Java cause chain, and one failed input does
not stop the others. The exit status is 0 when every input succeeded, 1 when at least one failed and 2 on a usage
error.

## JSON output

`--format json` writes one object per input per line, following the versioned schema in
[`schema/output-v1.schema.json`](schema/output-v1.schema.json):

```json
{"schema_version":1,
 "source":{"path":"report.pdf","size":266240,"blake3":"0b9b01a7..."},
 "text":"...",
 "mime_type":"application/pdf",
 "status":"complete",
 "errors":[],
 "metadata":{"title":"...","authors":["..."],"created":{"utc":"2022-07-29T10:03:21Z"},"page_count":28}}
```

`metadata` (with `-m`) holds normalised fields filled from whichever Tika key a format uses: dates are RFC 3339 in UTC,
or marked `unknown_offset` when the document gave no offset; multi-valued fields are always arrays. Tika's own keys are
available with `--raw-metadata`. `status` is `complete`, `partial` (some text before an error) or `failed`, and every
error carries a category and its full cause chain.

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
| `just lint` | Clippy over every target, warnings fail |
| `just fmt` | Format every Rust source |
| `just bench` | Run the criterion benchmarks |
| `just tika-agent` | Run Tika under the GraalVM tracing agent to regenerate native-image metadata |

## License

Apache License 2.0, see [LICENSE](LICENSE). Originally developed by [Yobix AI](https://yobix.ai).
