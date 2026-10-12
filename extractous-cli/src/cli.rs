use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::normalize::Step;
use crate::tree::Limits;

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

    /// Print the licence of extractous and of the third-party software compiled into it.
    #[arg(long, exclusive = true)]
    pub licenses: bool,

    #[command(flatten)]
    pub extract: ExtractArgs,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print the media type Apache Tika detects for each input: the type alone for a single
    /// input, `PATH: TYPE` lines for several.
    Detect(InputArgs),
    /// Run an HTTP server: POST a document to /extract or /detect.
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Address and port to listen on; port 0 picks a free one.
    #[arg(long, default_value = "127.0.0.1:9998", env = "EXTRACTOUS_LISTEN")]
    pub listen: std::net::SocketAddr,

    /// Extraction worker threads.
    #[arg(long, env = "EXTRACTOUS_WORKERS", default_value_t = default_jobs())]
    pub workers: NonZeroUsize,

    /// Requests that may wait for a worker before new ones get 503.
    #[arg(long, env = "EXTRACTOUS_QUEUE", default_value_t = 16)]
    pub queue: usize,

    /// Largest accepted request body in bytes; larger uploads get 413. Unlimited by default.
    #[arg(long, value_name = "BYTES", env = "EXTRACTOUS_MAX_INPUT_BYTES")]
    pub max_input_bytes: Option<u64>,

    /// Seconds a request may take from upload to the end of its response; slower ones get 504
    /// or are cut off.
    #[arg(long, value_name = "SECONDS", env = "EXTRACTOUS_TIMEOUT")]
    pub timeout: Option<u64>,

    #[command(flatten)]
    pub runtime: RuntimeArgs,

    #[command(flatten)]
    pub cache: CacheArgs,

    /// Seconds in-flight requests get to finish after a shutdown signal.
    #[arg(
        long,
        value_name = "SECONDS",
        env = "EXTRACTOUS_SHUTDOWN_GRACE",
        default_value_t = 30
    )]
    pub shutdown_grace: u64,
}

/// Where the output of complete extractions is kept for reuse.
#[derive(Debug, Args)]
pub struct CacheArgs {
    /// Keep the output of complete extractions in DIR and reuse it while the input, the options
    /// and the extractous build are unchanged. Extractions with passwords are never cached, so
    /// decrypted text never lands on disk, and neither are separate embedded documents.
    #[arg(long, value_name = "DIR", env = "EXTRACTOUS_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Evict the least recently used cache entries whenever the cache grows past this many
    /// bytes.
    #[arg(
        long,
        value_name = "BYTES",
        env = "EXTRACTOUS_CACHE_MAX_SIZE",
        requires = "cache_dir"
    )]
    pub cache_max_size: Option<u64>,
}

/// Where extraction keeps temporary files and how much memory it may use.
#[derive(Debug, Args)]
pub struct RuntimeArgs {
    /// Directory for temporary files: spooled input and output, embedded documents, and Tika's
    /// scratch files for large PDFs. The system temporary directory by default.
    #[arg(long, value_name = "DIR", env = "EXTRACTOUS_SPOOL_DIR")]
    pub spool_dir: Option<PathBuf>,

    /// The most memory Tika's Java heap may take. An extraction that needs more fails with a
    /// limit-exceeded error, and the next one runs normally. By default the heap may grow to a
    /// large share of physical memory.
    #[arg(long, value_name = "BYTES", env = "EXTRACTOUS_MAX_HEAP_BYTES")]
    pub max_heap_bytes: Option<NonZeroU64>,
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

    /// Output the text exactly as Tika extracted it, with no normalisation step.
    #[arg(long, env = "EXTRACTOUS_RAW", conflicts_with = "no_normalize")]
    pub raw: bool,

    /// A normalisation step to leave out; repeat or separate with commas. Text, json, pages and
    /// elements output is normalised by every step by default; xhtml never is.
    #[arg(
        long,
        value_enum,
        value_name = "STEP",
        value_delimiter = ',',
        env = "EXTRACTOUS_NO_NORMALIZE"
    )]
    pub no_normalize: Vec<Step>,

    /// Fail (exit status 1) when any part of an input could not be extracted, not only when
    /// nothing could.
    #[arg(long, env = "EXTRACTOUS_STRICT")]
    pub strict: bool,

    /// A password to try on encrypted documents; repeat for more. Command-line arguments are
    /// visible to other users in process listings, so prefer --password-file or the
    /// EXTRACTOUS_PASSWORD environment variable.
    #[arg(
        long = "password",
        value_name = "PASSWORD",
        env = "EXTRACTOUS_PASSWORD",
        hide_env_values = true
    )]
    pub passwords: Vec<String>,

    /// A file of passwords to try on encrypted documents, one per line, after any --password.
    #[arg(long, value_name = "FILE", env = "EXTRACTOUS_PASSWORD_FILE")]
    pub password_file: Option<PathBuf>,

    /// Seconds each input may take; when they run out, the text extracted so far is kept and
    /// the input is reported as partial (or failed, if there was none).
    #[arg(long, value_name = "SECONDS", env = "EXTRACTOUS_TIMEOUT")]
    pub timeout: Option<u64>,

    /// How documents embedded in an input (attachments, archive members, embedded objects) are
    /// handled: inline adds their text to the input's, separate extracts each as a document of
    /// its own, recursively, with its path inside the input and its hash, referencing repeats.
    #[arg(long, value_enum, default_value_t = Embedded::Inline, env = "EXTRACTOUS_EMBEDDED")]
    pub embedded: Embedded,

    /// With --embedded separate: how deep to descend into embedded documents.
    #[arg(
        long,
        value_name = "N",
        default_value_t = 16,
        env = "EXTRACTOUS_MAX_DEPTH"
    )]
    pub max_depth: usize,

    /// With --embedded separate: how many embedded documents one input may contain.
    #[arg(
        long,
        value_name = "N",
        default_value_t = 10_000,
        env = "EXTRACTOUS_MAX_EMBEDDED"
    )]
    pub max_embedded: usize,

    /// With --embedded separate: how many bytes the embedded documents of one input may expand
    /// to in all, which stops decompression bombs.
    #[arg(long, value_name = "BYTES", default_value_t = Limits::DEFAULT.max_expanded_bytes, env = "EXTRACTOUS_MAX_EXPANDED_BYTES")]
    pub max_expanded_bytes: u64,

    #[command(flatten)]
    pub runtime: RuntimeArgs,

    #[command(flatten)]
    pub cache: CacheArgs,

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
pub enum Embedded {
    Inline,
    Separate,
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
