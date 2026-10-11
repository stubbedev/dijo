use std::num::NonZeroUsize;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "extractous",
    version,
    about = "Extract text and metadata from documents with Apache Tika compiled to native code",
    long_about = "Extract text and metadata from documents with Apache Tika compiled to native code.\n\n\
                  Every option can also be set through the environment variable shown with it.\n\n\
                  Exit status: 0 when text was extracted from every input, 1 when nothing could be \
                  extracted from at least one (or, with --strict, when any part of one failed), 2 \
                  on a usage error.",
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub extract: ExtractArgs,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the media type Apache Tika detects for each input: the type alone for a single
    /// input, `PATH: TYPE` lines for several.
    Detect(InputArgs),
}

#[derive(Debug, Args)]
pub struct InputArgs {
    /// Files, directories (with --recursive) or - for standard input.
    #[arg(value_name = "INPUT", required = true)]
    pub inputs: Vec<PathBuf>,

    /// Descend into directories, visiting entries in sorted order.
    #[arg(short, long, env = "EXTRACTOUS_RECURSIVE")]
    pub recursive: bool,
}

#[derive(Debug, Args)]
pub struct ExtractArgs {
    #[command(flatten)]
    pub input: InputArgs,

    /// Output format.
    #[arg(short, long, value_enum, default_value_t = Format::Text, env = "EXTRACTOUS_FORMAT")]
    pub format: Format,

    /// Include metadata: Tika's keys on standard error for text and xhtml, the normalised fields
    /// inline for json, pages and elements.
    #[arg(short, long, env = "EXTRACTOUS_METADATA")]
    pub metadata: bool,

    /// Include Tika's raw metadata keys and values in json, pages and elements output.
    #[arg(long, env = "EXTRACTOUS_RAW_METADATA")]
    pub raw_metadata: bool,

    /// Fail (exit status 1) when any part of an input could not be extracted, not only when
    /// nothing could.
    #[arg(long, env = "EXTRACTOUS_STRICT")]
    pub strict: bool,

    /// Write one file per input into DIR, mirroring the input paths, instead of standard output.
    #[arg(short, long, value_name = "DIR", env = "EXTRACTOUS_OUTPUT")]
    pub output: Option<PathBuf>,

    /// Number of inputs extracted in parallel. Output stays in input order.
    #[arg(short, long, env = "EXTRACTOUS_JOBS", default_value_t = default_jobs())]
    pub jobs: NonZeroUsize,

    /// When to run OCR: on PDF pages and images without a text layer, never, or on every PDF
    /// page in addition to its text layer.
    #[arg(long, value_enum, default_value_t = Ocr::Auto, env = "EXTRACTOUS_OCR")]
    pub ocr: Ocr,

    /// Tesseract languages for OCR, such as `deu` or `eng+ara`.
    #[arg(long, value_name = "LANGS", env = "EXTRACTOUS_OCR_LANG")]
    pub ocr_lang: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Plain text.
    Text,
    /// Apache Tika's XHTML.
    Xhtml,
    /// One JSON object per input, one per line, following schema/output-v1.schema.json.
    Json,
    /// One JSON object per page (or the whole text when the format has no pages), then one
    /// object per input with its status and metadata.
    Pages,
    /// One JSON object per heading, paragraph, list item or table, with its page, then one
    /// object per input with its status and metadata.
    Elements,
}

impl Format {
    pub fn file_extension(self) -> &'static str {
        match self {
            Format::Text => "txt",
            Format::Xhtml => "xhtml",
            Format::Json => "json",
            Format::Pages | Format::Elements => "ndjson",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Ocr {
    Auto,
    Never,
    Always,
}

fn default_jobs() -> NonZeroUsize {
    std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN)
}
