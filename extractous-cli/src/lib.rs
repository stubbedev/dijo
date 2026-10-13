//! The extractous command line: one-shot extraction and the HTTP server. [`fuzz`] exposes the
//! parsers of untrusted input to the coverage-guided fuzz targets in `fuzz/`.

mod batch;
mod cache;
mod cli;
mod inputs;
mod language;
mod normalize;
mod render;
mod running;
mod schema;
mod server;
mod settings;
mod spool;
mod structure;
mod tree;

pub mod fuzz;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, OnceLock};
use std::thread;
use std::time::Duration;

use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use extractous_core::{Extractor, IsolateSettings, Metadata, Password};

use cache::{Cache, OutputSettings};
use cli::{Cli, Command, Embedded, ExtractArgs, Format, InputArgs, Isolation, ServeArgs};
use inputs::{Input, InputError, Source};
use normalize::Steps;
use render::RenderOptions;
use schema::Status;
use spool::Spool;

const LICENSES: &str = concat!(
    include_str!("../../LICENSE"),
    "\n",
    include_str!("../../THIRD-PARTY-LICENSES.md")
);

/// Runs the command line on the process's arguments.
pub fn main() -> ExitCode {
    if run_program(std::env::args_os().collect()) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The entry point of the statically linked executable, which the Java `main` of the native
/// image calls with its command line: 0 on success, 1 on failure, as `main` exits with.
#[cfg(feature = "static-image")]
#[unsafe(no_mangle)]
pub extern "C" fn extractous_main(
    argc: std::ffi::c_int,
    argv: *const *const std::ffi::c_char,
) -> std::ffi::c_int {
    use std::os::unix::ffi::OsStrExt;
    let arguments = (0..usize::try_from(argc).unwrap_or(0))
        .map(|index| {
            let argument = unsafe { std::ffi::CStr::from_ptr(*argv.add(index)) };
            std::ffi::OsStr::from_bytes(argument.to_bytes()).to_os_string()
        })
        .collect();
    std::ffi::c_int::from(!run_program(arguments))
}

/// musl's allocator, which the static executable would otherwise use, is slow under
/// concurrent allocation.
#[cfg(feature = "static-image")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

static ARGUMENTS: OnceLock<Vec<OsString>> = OnceLock::new();

/// The arguments the program was run with, without the program name: those a worker process is
/// started with too.
fn arguments() -> &'static [OsString] {
    ARGUMENTS
        .get()
        .map_or(&[], |arguments| arguments.get(1..).unwrap_or(&[]))
}

fn run_program(arguments: Vec<OsString>) -> bool {
    let cli = Cli::parse_from(&arguments);
    ARGUMENTS
        .set(arguments)
        .expect("the command line runs once per process");
    if let Some(conflict) = usage_conflict(&cli) {
        Cli::command()
            .error(ErrorKind::ArgumentConflict, conflict)
            .exit();
    }
    let succeeded = if cli.licenses {
        print_licenses()
    } else {
        run_command(cli)
    };
    wait_for_stopping_extractions();
    succeeded
}

/// How long to wait at exit for cancelled extractions to stop, so the tesseract processes they
/// started do not outlive this one.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// Why the arguments cannot be combined, if they cannot.
fn usage_conflict(cli: &Cli) -> Option<&'static str> {
    match &cli.command {
        None => cache_conflict(&cli.extract),
        Some(Command::Serve(args)) => isolation_conflict(args),
        Some(Command::Detect(_) | Command::Worker(_)) => None,
    }
}

/// Why the server arguments cannot limit worker memory, if they ask to: only a worker process can
/// be stopped on its own, and only Linux reports its resident memory cheaply.
fn isolation_conflict(args: &ServeArgs) -> Option<&'static str> {
    args.worker_max_rss_bytes?;
    if args.isolation != Isolation::Process {
        Some("--worker-max-rss-bytes requires --isolation process")
    } else if !cfg!(target_os = "linux") {
        Some("--worker-max-rss-bytes is only supported on Linux")
    } else {
        None
    }
}

/// Why the extraction arguments cannot use the cache, if they ask for it: decrypted text must not
/// land on disk.
fn cache_conflict(args: &ExtractArgs) -> Option<&'static str> {
    args.cache.cache_dir.as_ref()?;
    (!args.passwords.is_empty() || args.password_file.is_some())
        .then_some("--cache-dir cannot be combined with --password or --password-file")
}

fn render_options(args: &ExtractArgs) -> RenderOptions {
    RenderOptions {
        format: args.format,
        metadata: args.metadata,
        raw_metadata: args.raw_metadata,
        normalization: Steps::selected(args.raw, &args.no_normalize),
    }
}

/// What, besides each input, decides the output of these extraction arguments.
fn output_settings(args: &ExtractArgs) -> OutputSettings<'_> {
    OutputSettings {
        render: render_options(args),
        ocr: args.ocr,
        ocr_lang: args.ocr_lang.as_deref(),
        embedded: args.embedded,
    }
}

fn wait_for_stopping_extractions() {
    match extractous_core::wait_for_extractions(STOP_GRACE) {
        Ok(true) => {}
        Ok(false) => report(
            "extractous",
            &format!(
                "an extraction was still stopping after {}s",
                STOP_GRACE.as_secs()
            ),
        ),
        Err(error) => report("extractous", &error.to_string()),
    }
}

fn print_licenses() -> bool {
    let mut stdout = io::stdout().lock();
    let written = stdout.write_all(LICENSES.as_bytes()).and_then(|()| {
        match extractous_core::bundled_ocr_licenses() {
            Some(licenses) => stdout
                .write_all(
                    b"\n# Bundled OCR: tesseract, the libraries it links and its language data\n\n",
                )
                .and_then(|()| stdout.write_all(licenses.as_bytes())),
            None => Ok(()),
        }
    });
    written.map_or_else(|error| stdout_failed(&error), |()| true)
}

fn run_command(cli: Cli) -> bool {
    let runtime = match &cli.command {
        Some(Command::Serve(args)) => Some(&args.runtime),
        Some(Command::Worker(args)) => Some(&args.runtime),
        Some(Command::Detect(_)) => None,
        None => Some(&cli.extract.runtime),
    };
    if let Some(runtime) = runtime {
        let settings = IsolateSettings {
            temp_dir: runtime.spool_dir.clone(),
            max_heap_bytes: runtime.max_heap_bytes,
        };
        if let Err(error) = extractous_core::configure_isolate(settings) {
            report("extractous", &error.to_string());
            return false;
        }
    }
    match cli.command {
        Some(Command::Detect(args)) => detect(&args),
        Some(Command::Serve(args)) => server::serve(&args),
        Some(Command::Worker(args)) => server::work(&args),
        None => match &cli.extract.batch_worker {
            Some(spool_dir) => batch::serve(&cli.extract, spool_dir),
            None => extract(&cli.extract),
        },
    }
}

fn report(display: &str, message: &str) {
    eprintln!("extractous: {display}: {message}");
}

/// An input together with the file on disk Tika reads it from.
struct Resolved {
    input: Input,
    path: PathBuf,
}

/// Resolves every input to a file on disk. Standard input is copied once into the spool, since
/// Tika needs random access for many formats.
fn resolve_inputs(
    args: &InputArgs,
    spool: &Spool,
) -> Result<Vec<Result<Resolved, InputError>>, String> {
    let mut stdin_path: Option<PathBuf> = None;
    let mut resolved = Vec::new();
    for input in inputs::expand(&args.inputs, args.recursive) {
        resolved.push(match input {
            Err(error) => Err(error),
            Ok(input) => match &input.source {
                Source::File(path) => {
                    let path = path.clone();
                    Ok(Resolved { input, path })
                }
                Source::Stdin if stdin_path.is_some() => Err(InputError {
                    display: input.display,
                    message: "standard input can only be read once".to_string(),
                }),
                Source::Stdin => {
                    let path = spool
                        .copy_stdin()
                        .map_err(|e| format!("Failed to read standard input: {e}"))?;
                    stdin_path = Some(path.clone());
                    Ok(Resolved { input, path })
                }
            },
        });
    }
    Ok(resolved)
}

fn detect(args: &InputArgs) -> bool {
    let spool = match Spool::create(None) {
        Ok(spool) => spool,
        Err(error) => {
            report("spool", &error.to_string());
            return false;
        }
    };
    let inputs = match resolve_inputs(args, &spool) {
        Ok(inputs) => inputs,
        Err(error) => {
            report("-", &error);
            return false;
        }
    };
    let single = inputs.len() == 1;
    let extractor = Extractor::new();
    let mut stdout = io::stdout().lock();
    let mut succeeded = true;
    for input in inputs {
        let (display, detected) = match input {
            Err(error) => (error.display, Err(error.message)),
            Ok(Resolved { input, path }) => (input.display, detect_one(&extractor, &path)),
        };
        let written = match detected {
            Ok(media_type) if single => writeln!(stdout, "{media_type}"),
            Ok(media_type) => writeln!(stdout, "{display}: {media_type}"),
            Err(message) => {
                report(&display, &message);
                succeeded = false;
                Ok(())
            }
        };
        if let Err(error) = written {
            return stdout_failed(&error);
        }
    }
    succeeded
}

fn detect_one(extractor: &Extractor, path: &Path) -> Result<String, String> {
    let path = path
        .to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))?;
    extractor
        .detect_file(path)
        .map(|media_type| media_type.to_string())
        .map_err(|e| e.to_string())
}

fn stdout_failed(error: &io::Error) -> bool {
    if error.kind() != io::ErrorKind::BrokenPipe {
        report("standard output", &error.to_string());
    }
    false
}

/// The `--password` values in order, then the lines of `--password-file`.
fn read_passwords(args: &ExtractArgs) -> Result<Vec<String>, String> {
    let mut passwords = args.passwords.clone();
    if let Some(path) = &args.password_file {
        let contents = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        passwords.extend(
            contents
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string),
        );
    }
    Ok(passwords)
}

/// The extractor and cache every input of a run is extracted with, or what failed and why.
fn prepare(
    args: &ExtractArgs,
    passwords: Vec<String>,
) -> Result<(Extractor, Option<Cache>), (&'static str, String)> {
    let extractor = settings::extractor(
        render_options(args),
        args.ocr,
        args.ocr_lang.as_deref(),
        passwords.iter().map(Password::new).collect(),
        args.timeout.map(Duration::from_secs),
    );
    let cache = Cache::from_args(&args.cache, args.cache_trust_mtime)
        .map_err(|error| ("--cache-dir", error))?;
    Ok((extractor, cache))
}

/// Where one input's output goes.
enum Sink {
    Stdout,
    Spooled(PathBuf),
    OutputFile(PathBuf),
}

/// The result of one input, emitted in input order by the main thread.
struct Finished {
    display: String,
    output: Result<Option<PathBuf>, SinkFailure>,
    metadata: Option<Metadata>,
    status: Status,
    errors: Vec<String>,
}

fn extract(args: &ExtractArgs) -> bool {
    let spool = match Spool::create(args.runtime.spool_dir.as_deref()) {
        Ok(spool) => spool,
        Err(error) => {
            report("spool", &error.to_string());
            return false;
        }
    };
    let inputs = match resolve_inputs(&args.input, &spool) {
        Ok(inputs) => inputs,
        Err(error) => {
            report("-", &error);
            return false;
        }
    };
    let passwords = match read_passwords(args) {
        Ok(passwords) => passwords,
        Err(error) => {
            report("--password-file", &error);
            return false;
        }
    };
    let workers = args.jobs.get().min(inputs.len().max(1));
    let processes = workers.div_ceil(batch::JOBS_PER_PROCESS);
    let (sender, receiver) = mpsc::channel::<(usize, Finished)>();
    if processes > 1 {
        return thread::scope(|scope| {
            let (inputs, spool) = (&inputs, &spool);
            scope.spawn(move || {
                batch::extract(args, inputs, spool.dir(), &passwords, processes, &sender);
            });
            emit_in_order(receiver, args.format, args.strict)
        });
    }
    let (extractor, cache) = match prepare(args, passwords) {
        Ok(prepared) => prepared,
        Err((what, error)) => {
            report(what, &error);
            return false;
        }
    };
    let context = Context {
        extractor: &extractor,
        args,
        options: render_options(args),
        cache: cache.as_ref(),
        spool_dir: spool.dir(),
        stream_to_stdout: workers == 1,
    };
    let next = AtomicUsize::new(0);

    let succeeded = thread::scope(|scope| {
        for _ in 0..workers {
            let sender = sender.clone();
            let (inputs, next, context) = (&inputs, &next, &context);
            scope.spawn(move || loop {
                if STDOUT_CLOSED.load(Ordering::Relaxed) {
                    break;
                }
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(input) = inputs.get(index) else {
                    break;
                };
                let finished = run(context, index, input);
                if sender.send((index, finished)).is_err() {
                    break;
                }
            });
        }
        drop(sender);
        emit_in_order(receiver, args.format, args.strict)
    });
    succeeded
}

/// What every input of one run is extracted with.
struct Context<'a> {
    extractor: &'a Extractor,
    args: &'a ExtractArgs,
    options: RenderOptions,
    cache: Option<&'a Cache>,
    spool_dir: &'a Path,
    /// Whether output goes straight to standard output, as it can when one input is extracted
    /// at a time, rather than through the spool.
    stream_to_stdout: bool,
}

fn run(context: &Context, index: usize, input: &Result<Resolved, InputError>) -> Finished {
    let Context {
        extractor,
        args,
        options,
        cache,
        spool_dir,
        stream_to_stdout,
    } = *context;
    let Resolved { input, path } = match input {
        Ok(resolved) => resolved,
        Err(error) => {
            return Finished {
                display: error.display.clone(),
                output: Ok(None),
                metadata: None,
                status: Status::Failed,
                errors: vec![error.message.clone()],
            }
        }
    };
    let sink = sink(args, spool_dir, index, input, stream_to_stdout);
    let keep_failed = matches!(args.format, Format::Json | Format::Pages | Format::Elements);
    let rendered = write_sink(&sink, keep_failed, |writer| match args.embedded {
        Embedded::Inline => {
            let render = |writer: &mut dyn Write| {
                render::render(extractor, path, &input.display, options, writer)
            };
            match cache {
                Some(cache) => {
                    let source = render::source(path, &input.display, None);
                    cache.render(
                        path,
                        &input.display,
                        &source,
                        output_settings(args),
                        writer,
                        render,
                    )
                }
                None => render(writer),
            }
        }
        Embedded::Separate => {
            let scratch = embedded_scratch(spool_dir, index);
            fs::create_dir(&scratch)?;
            let limits = tree::Limits {
                max_depth: args.max_depth,
                max_documents: args.max_embedded,
                max_expanded_bytes: args.max_expanded_bytes,
            };
            let rendered = tree::render_tree(
                extractor,
                path,
                &input.display,
                options,
                limits,
                &scratch,
                cache.map(|cache| (cache, output_settings(args))),
                writer,
            );
            fs::remove_dir_all(&scratch)?;
            rendered
        }
    });
    match rendered {
        Ok(outcome) => Finished {
            display: input.display.clone(),
            output: Ok(match sink {
                Sink::Spooled(path) => Some(path),
                Sink::Stdout | Sink::OutputFile(_) => None,
            }),
            metadata: outcome.metadata,
            status: outcome.status,
            errors: outcome
                .errors
                .into_iter()
                .map(|error| error.message)
                .collect(),
        },
        Err(error) => Finished {
            display: input.display.clone(),
            output: Err(error),
            metadata: None,
            status: Status::Failed,
            errors: Vec::new(),
        },
    }
}

fn sink(
    args: &ExtractArgs,
    spool_dir: &Path,
    index: usize,
    input: &Input,
    stream_to_stdout: bool,
) -> Sink {
    match &args.output {
        Some(dir) => Sink::OutputFile(
            dir.join(&input.output_path)
                .with_added_extension(args.format.file_extension()),
        ),
        None if stream_to_stdout => Sink::Stdout,
        None => Sink::Spooled(spool_dir.join(format!("{index}.out"))),
    }
}

/// Where the embedded documents of input `index` are kept while they are extracted.
fn embedded_scratch(spool_dir: &Path, index: usize) -> PathBuf {
    spool_dir.join(format!("{index}.embedded"))
}

/// The file an output file is written to by process `pid` before it is renamed into place.
fn partial_output(path: &Path, pid: u32) -> PathBuf {
    path.with_added_extension(format!("partial-{pid}"))
}

/// Why an input's output could not be written.
enum SinkFailure {
    StdoutClosed,
    Failed(String),
}

/// Set once standard output is closed by its reader, after which nothing more is extracted.
static STDOUT_CLOSED: AtomicBool = AtomicBool::new(false);

fn stdout_closed(error: &io::Error) -> bool {
    let closed = error.kind() == io::ErrorKind::BrokenPipe;
    if closed {
        STDOUT_CLOSED.store(true, Ordering::Relaxed);
    }
    closed
}

fn write_sink(
    sink: &Sink,
    keep_failed: bool,
    render: impl FnOnce(&mut dyn Write) -> io::Result<render::Outcome>,
) -> Result<render::Outcome, SinkFailure> {
    let (target, written) = match sink {
        Sink::Stdout => {
            let mut stdout = BufWriter::new(io::stdout().lock());
            let written = render(&mut stdout).and_then(|outcome| stdout.flush().map(|()| outcome));
            ("standard output".to_string(), written)
        }
        Sink::Spooled(path) => (path.display().to_string(), write_file(path, render)),
        Sink::OutputFile(path) => (
            path.display().to_string(),
            write_file_atomically(path, keep_failed, render),
        ),
    };
    written.map_err(|error| {
        if matches!(sink, Sink::Stdout) && stdout_closed(&error) {
            SinkFailure::StdoutClosed
        } else {
            SinkFailure::Failed(format!("Failed to write {target}: {error}"))
        }
    })
}

fn write_file(
    path: &Path,
    render: impl FnOnce(&mut dyn Write) -> io::Result<render::Outcome>,
) -> io::Result<render::Outcome> {
    let mut file = BufWriter::new(fs::File::create_new(path)?);
    let outcome = render(&mut file)?;
    file.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(outcome)
}

fn write_file_atomically(
    path: &Path,
    keep_failed: bool,
    render: impl FnOnce(&mut dyn Write) -> io::Result<render::Outcome>,
) -> io::Result<render::Outcome> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let partial = partial_output(path, std::process::id());
    let written = write_file(&partial, render);
    match written {
        Ok(outcome)
            if outcome.status == Status::Complete
                || outcome.status == Status::Partial
                || keep_failed =>
        {
            fs::rename(&partial, path)?;
            Ok(outcome)
        }
        result => {
            fs::remove_file(&partial)?;
            result
        }
    }
}

fn emit_in_order(
    receiver: mpsc::Receiver<(usize, Finished)>,
    format: Format,
    strict: bool,
) -> bool {
    let mut pending = BTreeMap::new();
    let mut next = 0;
    let mut succeeded = true;
    for (index, finished) in receiver {
        pending.insert(index, finished);
        while let Some(finished) = pending.remove(&next) {
            succeeded &= !STDOUT_CLOSED.load(Ordering::Relaxed) && emit(finished, format, strict);
            next += 1;
        }
    }
    succeeded
}

fn emit(finished: Finished, format: Format, strict: bool) -> bool {
    let mut succeeded = match finished.status {
        Status::Complete => true,
        Status::Partial => !strict,
        Status::Encrypted | Status::Failed => false,
    };
    match finished.output {
        Ok(Some(spooled)) => {
            let copied = fs::File::open(&spooled)
                .and_then(|mut file| io::copy(&mut file, &mut io::stdout().lock()))
                .and_then(|_| fs::remove_file(&spooled));
            if let Err(error) = copied {
                succeeded = false;
                if !stdout_closed(&error) {
                    report("standard output", &error.to_string());
                }
            }
        }
        Ok(None) => {}
        Err(SinkFailure::StdoutClosed) => succeeded = false,
        Err(SinkFailure::Failed(error)) => {
            succeeded = false;
            report(&finished.display, &error);
        }
    }
    if let (Some(metadata), Format::Text | Format::Xhtml) = (&finished.metadata, format) {
        let mut stderr = io::stderr().lock();
        let _ = writeln!(stderr, "==> {} <==", finished.display);
        for (key, values) in metadata.iter().collect::<BTreeMap<_, _>>() {
            for value in values {
                let _ = writeln!(stderr, "{key}: {value}");
            }
        }
    }
    for error in &finished.errors {
        report(&finished.display, error);
    }
    succeeded
}
