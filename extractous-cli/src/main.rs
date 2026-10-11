mod cli;
mod inputs;
mod render;
mod schema;
mod spool;

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;

use clap::Parser;
use extractous_core::{Extractor, Metadata, PdfOcrStrategy, PdfParserConfig, TesseractOcrConfig};

use cli::{Cli, Command, ExtractArgs, Format, InputArgs, Ocr};
use inputs::{Input, InputError, Source};
use render::RenderOptions;
use schema::Status;
use spool::Spool;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let succeeded = match cli.command {
        Some(Command::Detect(args)) => detect(&args),
        None => extract(&cli.extract),
    };
    if succeeded {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
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
    let spool = match Spool::create() {
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

fn configure(args: &ExtractArgs) -> Extractor {
    let (strategy, skip_ocr) = match args.ocr {
        Ocr::Auto => (PdfOcrStrategy::AUTO, false),
        Ocr::Never => (PdfOcrStrategy::NO_OCR, true),
        Ocr::Always => (PdfOcrStrategy::OCR_AND_TEXT_EXTRACTION, false),
    };
    let mut ocr = TesseractOcrConfig::new().set_skip_ocr(skip_ocr);
    if let Some(languages) = &args.ocr_lang {
        ocr = ocr.set_language(languages.as_str());
    }
    Extractor::new()
        .set_xml_output(args.format == Format::Xhtml)
        .set_pdf_config(PdfParserConfig::new().set_ocr_strategy(strategy))
        .set_ocr_config(ocr)
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
    output: Result<Option<PathBuf>, String>,
    metadata: Option<Metadata>,
    status: Status,
    errors: Vec<String>,
}

fn extract(args: &ExtractArgs) -> bool {
    let spool = match Spool::create() {
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
    let extractor = configure(args);
    let workers = args.jobs.get().min(inputs.len().max(1));
    let next = AtomicUsize::new(0);
    let (sender, receiver) = mpsc::channel::<(usize, Finished)>();

    thread::scope(|scope| {
        for _ in 0..workers {
            let sender = sender.clone();
            let (inputs, next, extractor, spool) = (&inputs, &next, &extractor, &spool);
            scope.spawn(move || loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(input) = inputs.get(index) else {
                    break;
                };
                let finished = run(extractor, args, spool, workers, index, input);
                if sender.send((index, finished)).is_err() {
                    break;
                }
            });
        }
        drop(sender);
        emit_in_order(receiver, args.format)
    })
}

fn run(
    extractor: &Extractor,
    args: &ExtractArgs,
    spool: &Spool,
    workers: usize,
    index: usize,
    input: &Result<Resolved, InputError>,
) -> Finished {
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
    let sink = match &args.output {
        Some(dir) => Sink::OutputFile(
            dir.join(&input.output_path)
                .with_added_extension(args.format.file_extension()),
        ),
        None if workers == 1 => Sink::Stdout,
        None => Sink::Spooled(spool.path(&format!("{index}.out"))),
    };
    let keep_failed = args.format == Format::Json;
    let rendered = write_sink(&sink, keep_failed, |writer| {
        render::render(
            extractor,
            path,
            &input.display,
            RenderOptions {
                format: args.format,
                metadata: args.metadata,
                raw_metadata: args.raw_metadata,
            },
            writer,
        )
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

fn write_sink(
    sink: &Sink,
    keep_failed: bool,
    render: impl FnOnce(&mut dyn Write) -> io::Result<render::Outcome>,
) -> Result<render::Outcome, String> {
    match sink {
        Sink::Stdout => {
            let mut stdout = BufWriter::new(io::stdout().lock());
            render(&mut stdout)
                .and_then(|outcome| stdout.flush().map(|()| outcome))
                .map_err(|e| sink_error("standard output", &e))
        }
        Sink::Spooled(path) => {
            write_file(path, render).map_err(|e| sink_error(&path.display().to_string(), &e))
        }
        Sink::OutputFile(path) => write_file_atomically(path, keep_failed, render)
            .map_err(|e| sink_error(&path.display().to_string(), &e)),
    }
}

fn sink_error(target: &str, error: &io::Error) -> String {
    format!("Failed to write {target}: {error}")
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
    let partial = path.with_added_extension(format!("partial-{}", std::process::id()));
    let written = write_file(&partial, render);
    match written {
        Ok(outcome) if outcome.status == Status::Complete || keep_failed => {
            fs::rename(&partial, path)?;
            Ok(outcome)
        }
        result => {
            fs::remove_file(&partial)?;
            result
        }
    }
}

fn emit_in_order(receiver: mpsc::Receiver<(usize, Finished)>, format: Format) -> bool {
    let mut pending = BTreeMap::new();
    let mut next = 0;
    let mut succeeded = true;
    for (index, finished) in receiver {
        pending.insert(index, finished);
        while let Some(finished) = pending.remove(&next) {
            succeeded &= emit(finished, format);
            next += 1;
        }
    }
    succeeded
}

fn emit(finished: Finished, format: Format) -> bool {
    let mut succeeded = finished.status == Status::Complete;
    match finished.output {
        Ok(Some(spooled)) => {
            let copied = fs::File::open(&spooled)
                .and_then(|mut file| io::copy(&mut file, &mut io::stdout().lock()))
                .and_then(|_| fs::remove_file(&spooled));
            if let Err(error) = copied {
                succeeded = false;
                stdout_failed(&error);
            }
        }
        Ok(None) => {}
        Err(error) => {
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
