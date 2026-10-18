use std::ffi::OsString;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

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
    /// Serve extraction requests from `serve --isolation process` over standard input and
    /// output.
    #[command(hide = true)]
    Worker(WorkerArgs),
}

#[derive(Debug, Args)]
pub struct WorkerArgs {
    #[command(flatten)]
    pub runtime: RuntimeArgs,

    #[command(flatten)]
    pub cache: CacheArgs,
}

/// Where the server runs extractions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Isolation {
    /// In worker threads of the server process.
    Thread,
    /// In worker processes, restarted when one crashes, overruns --timeout or exceeds
    /// --worker-max-rss-bytes, without taking the server or other requests down.
    Process,
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    /// Address and port to listen on; port 0 picks a free one.
    #[arg(long, default_value = "127.0.0.1:9998", env = "EXTRACTOUS_LISTEN")]
    pub listen: std::net::SocketAddr,

    /// Extraction workers: threads, at most 4 by default since threads share one isolate, or
    /// with --isolation process, processes, one per core by default.
    #[arg(long, env = "EXTRACTOUS_WORKERS")]
    pub workers: Option<NonZeroUsize>,

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

    /// Seconds a client may take to send its request headers, and may go silent while uploading:
    /// a connection whose headers are late is closed, an upload that stalls gets 408. Bounds
    /// what slow or idle clients can hold, with or without --timeout.
    #[arg(
        long,
        value_name = "SECONDS",
        env = "EXTRACTOUS_CLIENT_TIMEOUT",
        default_value_t = NonZeroU64::new(60).expect("60 is not zero")
    )]
    pub client_timeout: NonZeroU64,

    #[command(flatten)]
    pub limits: EmbeddedLimitsArgs,

    #[command(flatten)]
    pub runtime: RuntimeArgs,

    #[command(flatten)]
    pub cache: CacheArgs,

    /// Where extractions run: in threads of the server process, or in worker processes that a
    /// crash, a runaway allocation or a hung parser cannot take the server down with.
    #[arg(long, value_enum, default_value_t = Isolation::Thread, env = "EXTRACTOUS_ISOLATION")]
    pub isolation: Isolation,

    /// With --isolation process, on Linux: restart a worker process whose resident memory grows
    /// past this many bytes, failing its request with 413.
    #[arg(long, value_name = "BYTES", env = "EXTRACTOUS_WORKER_MAX_RSS_BYTES")]
    pub worker_max_rss_bytes: Option<NonZeroU64>,

    /// Seconds in-flight requests get to finish after a shutdown signal.
    #[arg(
        long,
        value_name = "SECONDS",
        env = "EXTRACTOUS_SHUTDOWN_GRACE",
        default_value_t = 30
    )]
    pub shutdown_grace: u64,
}

/// Bounds on the documents embedded in an input, when each is extracted as a document of its
/// own.
#[derive(Debug, Args)]
pub struct EmbeddedLimitsArgs {
    /// With separate embedded documents: how deep to descend into them.
    #[arg(
        long,
        value_name = "N",
        default_value_t = Limits::DEFAULT.max_depth,
        env = "EXTRACTOUS_MAX_DEPTH"
    )]
    pub max_depth: usize,

    /// With separate embedded documents: how many one input may contain.
    #[arg(
        long,
        value_name = "N",
        default_value_t = Limits::DEFAULT.max_documents,
        env = "EXTRACTOUS_MAX_EMBEDDED"
    )]
    pub max_embedded: usize,

    /// With separate embedded documents: how many bytes those of one input may expand to in
    /// all, which stops decompression bombs.
    #[arg(
        long,
        value_name = "BYTES",
        default_value_t = Limits::DEFAULT.max_expanded_bytes,
        env = "EXTRACTOUS_MAX_EXPANDED_BYTES"
    )]
    pub max_expanded_bytes: u64,
}

impl EmbeddedLimitsArgs {
    pub fn limits(&self) -> Limits {
        Limits {
            max_depth: self.max_depth,
            max_documents: self.max_embedded,
            max_expanded_bytes: self.max_expanded_bytes,
        }
    }
}

/// Where the output of complete extractions is kept for reuse.
#[derive(Debug, Args)]
pub struct CacheArgs {
    /// Keep the output of complete extractions in DIR and reuse it while the input, the options
    /// and the extractous build are unchanged; with --embedded separate, for each embedded
    /// document on its own. Extractions with passwords are never cached, so decrypted text never
    /// lands on disk.
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

    /// Also keep partial results, such as a damaged archive's readable members. Results cut
    /// short by a timeout are never kept.
    #[arg(long, env = "EXTRACTOUS_CACHE_PARTIAL", requires = "cache_dir")]
    pub cache_partial: bool,
}

impl CacheArgs {
    /// These options as they are given on a command line.
    pub fn arguments(&self) -> Vec<OsString> {
        let mut arguments = Vec::new();
        if let Some(dir) = &self.cache_dir {
            arguments.extend(["--cache-dir".into(), dir.into()]);
        }
        if let Some(bytes) = self.cache_max_size {
            arguments.extend(["--cache-max-size".into(), bytes.to_string().into()]);
        }
        if self.cache_partial {
            arguments.push("--cache-partial".into());
        }
        arguments
    }
}

/// Set in a worker process's environment to the processors it may use.
pub const PROCESSORS_VARIABLE: &str = "EXTRACTOUS_PROCESSORS";

/// Where extraction keeps temporary files and how much memory it may use.
#[derive(Debug, Args)]
pub struct RuntimeArgs {
    /// Directory for temporary files: spooled input and output, embedded documents, and Tika's
    /// scratch files for large PDFs. The system temporary directory by default.
    #[arg(long, value_name = "DIR", env = "EXTRACTOUS_SPOOL_DIR")]
    pub spool_dir: Option<PathBuf>,

    /// The most memory an extraction may take: Tika's Java heap, and what a native parser
    /// allocates. An extraction that needs more fails with a limit-exceeded error, and the next
    /// one runs normally. GraalVM raises a cap below the least heap the runtime starts with, a
    /// few tens of MiB. By default the heap may grow to a large share of physical memory, and is
    /// sized to at least 512 MiB (or the cap, when lower) so that the collector runs less often.
    #[arg(long, value_name = "BYTES", env = "EXTRACTOUS_MAX_HEAP_BYTES")]
    pub max_heap_bytes: Option<NonZeroU64>,

    /// The processors this process may use, its share of the machine's as a worker process
    /// among others.
    #[arg(long, hide = true, value_name = "N", env = PROCESSORS_VARIABLE)]
    pub processors: Option<NonZeroUsize>,
}

impl RuntimeArgs {
    /// These options as they are given on a command line.
    pub fn arguments(&self) -> Vec<OsString> {
        let mut arguments = Vec::new();
        if let Some(dir) = &self.spool_dir {
            arguments.extend(["--spool-dir".into(), dir.into()]);
        }
        if let Some(bytes) = self.max_heap_bytes {
            arguments.extend(["--max-heap-bytes".into(), bytes.to_string().into()]);
        }
        arguments
    }
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

    #[command(flatten)]
    pub limits: EmbeddedLimitsArgs,

    #[command(flatten)]
    pub runtime: RuntimeArgs,

    #[command(flatten)]
    pub cache: CacheArgs,

    /// With --cache-dir: take an input as unchanged when its path, size and modification time
    /// match the last run, without hashing it again. Faster, but trusts modification times.
    #[arg(long, env = "EXTRACTOUS_CACHE_TRUST_MTIME", requires = "cache_dir")]
    pub cache_trust_mtime: bool,

    /// Write one file per input into DIR, mirroring the input paths, instead of standard output.
    #[arg(short, long, value_name = "DIR", env = "EXTRACTOUS_OUTPUT")]
    pub output: Option<PathBuf>,

    /// Number of inputs extracted in parallel, by default one per core. Past two, they run in
    /// worker processes of two each, since the extractions in a process share one isolate and
    /// contend in it. Output stays in input order.
    #[arg(short, long, env = "EXTRACTOUS_JOBS", default_value_t = cores())]
    pub jobs: NonZeroUsize,

    /// Run as a worker process of another extractous, extracting the jobs it sends into its
    /// spool directory DIR.
    #[arg(long, hide = true, value_name = "DIR", env = crate::batch::WORKER_VARIABLE)]
    pub batch_worker: Option<PathBuf>,

    /// When to run OCR: on PDF pages and images without a text layer, never, or on every PDF
    /// page in addition to its text layer.
    #[arg(long, value_enum, default_value_t = Ocr::Auto, env = "EXTRACTOUS_OCR")]
    pub ocr: Ocr,

    /// Tesseract languages for OCR, such as `deu` or `eng+ara`. By default each image's script is
    /// detected (with tesseract's `osd` data) and read with an installed model for it.
    #[arg(long, value_name = "LANGS", env = "EXTRACTOUS_OCR_LANG")]
    pub ocr_lang: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
pub enum Embedded {
    Inline,
    Separate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
pub enum Ocr {
    Auto,
    Never,
    Always,
}

/// The most extractions that run well in parallel in one isolate: past it they contend inside
/// the isolate, and the whole gets slower.
const SHARED_ISOLATE_JOBS: NonZeroUsize = NonZeroUsize::new(4).unwrap();

fn cores() -> NonZeroUsize {
    std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN)
}

/// The processors each worker process may use, when worker processes run `jobs` extractions at
/// once, `jobs_per_process` in each: the same share of the machine's for every extraction.
pub fn processors_per_process(jobs: NonZeroUsize, jobs_per_process: usize) -> NonZeroUsize {
    share(cores(), jobs, jobs_per_process)
}

fn share(processors: NonZeroUsize, jobs: NonZeroUsize, jobs_per_process: usize) -> NonZeroUsize {
    NonZeroUsize::new(processors.get() * jobs_per_process / jobs.get()).unwrap_or(NonZeroUsize::MIN)
}

fn shared_isolate_jobs() -> NonZeroUsize {
    cores().min(SHARED_ISOLATE_JOBS)
}

impl ServeArgs {
    /// The workers to run: as given, or by default as many as run well in parallel.
    pub fn workers(&self) -> NonZeroUsize {
        self.workers.unwrap_or_else(|| match self.isolation {
            Isolation::Thread => shared_isolate_jobs(),
            Isolation::Process => cores(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).unwrap()
    }

    #[test]
    fn worker_processes_share_the_processors_by_their_jobs() {
        assert_eq!(share(n(12), n(12), 2), n(2));
        assert_eq!(share(n(12), n(4), 2), n(6));
        assert_eq!(share(n(12), n(12), 1), n(1));
        assert_eq!(share(n(12), n(3), 1), n(4));
    }

    #[test]
    fn a_worker_process_keeps_a_processor_when_jobs_outnumber_them() {
        assert_eq!(share(n(4), n(16), 2), n(1));
        assert_eq!(share(n(4), n(16), 1), n(1));
    }
}
