//! What tika-server's endpoints answer from an upload, produced in a worker.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::time::Instant;

use dijo_core::{language, Extractor, Metadata};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zip::write::{SimpleFileOptions, StreamWriter};
use zip::{CompressionMethod, ZipWriter};

use super::iso639;
use crate::cli::Format;
use crate::render::{self, Opened, Outcome, RenderOptions};
use crate::schema::{self, ExtractionError, Source, Status};
use crate::server::handlers::{
    self, error_head, status_for, BodyWriter, ContentType, ExtractRequest, Files, Head, Report,
    Respond, BODY_CHUNK_SIZE,
};
use crate::server::upload;
use crate::settings;
use crate::tree::{self, Document, Records};

const CONTENT: &str = "tk:content";
const HANDLER_TYPE: &str = "tk:content-handler-type";
const CONTAINER_EXCEPTION: &str = "tk:exception:container-exception";
const EMBEDDED_EXCEPTION: &str = "tk:exception:embedded-exception";
const EMBEDDED_DEPTH: &str = "tk:embedded-depth";
const EMBEDDED_PATH: &str = "tk:embedded-resource-path";
const EMBEDDED_ID: &str = "tk:embedded-id";
const RESOURCE_NAME: &str = "tk:resource-name";
const CONTENT_TYPE: &str = "Content-Type";
/// As much of an upload as `/language` reads, in characters, as tika-server does.
const LANGUAGE_CHARS: usize = 100_000;
/// The extension `/unpack` names a document by when its type has none.
const UNKNOWN_EXTENSION: &str = ".bin";
const METADATA_SUFFIX: &str = ".metadata.json";

/// How a document's content is written into its object, as tika-server's content handlers write
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Handler {
    Text,
    Html,
    Xml,
    Markdown,
    Ignore,
}

impl Handler {
    /// The handler a path names, as in tika-server's `/tika/json/{handler}` and
    /// `/rmeta/{handler}`.
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "text" => Some(Handler::Text),
            "html" => Some(Handler::Html),
            "xml" => Some(Handler::Xml),
            "md" => Some(Handler::Markdown),
            "ignore" => Some(Handler::Ignore),
            _ => None,
        }
    }

    /// The format the content is extracted in; plain text, unwritten, for [`Handler::Ignore`].
    pub fn format(self) -> Format {
        match self {
            Handler::Text | Handler::Ignore => Format::Text,
            Handler::Html | Handler::Xml => Format::Xhtml,
            Handler::Markdown => Format::Markdown,
        }
    }

    fn tika_name(self) -> &'static str {
        match self {
            Handler::Text => "TEXT",
            Handler::Html => "HTML",
            Handler::Xml => "XML",
            Handler::Markdown => "MARKDOWN",
            Handler::Ignore => "IGNORE",
        }
    }
}

/// How `/meta` writes a document's metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MetaForm {
    Json,
    Csv,
}

/// How `/meta/{field}` writes one field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldForm {
    Json,
    Csv,
    /// The first value alone.
    Plain,
}

/// What a worker answers an upload with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Output {
    /// `/tika/json`: the document's metadata and content, its embedded documents' content
    /// inlined, as one object.
    Json(Handler),
    /// `/rmeta`: an object per document, the upload first, then each document embedded in it.
    Rmeta(Handler),
    /// `/meta`: the metadata alone.
    Meta(MetaForm),
    /// `/meta/{field}`: one field of the metadata.
    MetaField { field: String, form: FieldForm },
    /// `/unpack`: the embedded documents as a ZIP archive; `/unpack/all` adds the upload and
    /// every document's metadata.
    Unpack { all: bool },
    /// `/detect`: the media type, without a line end.
    Detect,
    /// `/language`: the language of the upload, which is text.
    Language,
}

type Object = BTreeMap<String, Value>;

pub fn run(request: &ExtractRequest, output: &Output, files: &Files, respond: &mut dyn Respond) {
    let answered = match output {
        Output::Json(handler) => json(request, *handler, files, respond),
        Output::Rmeta(handler) => rmeta(request, *handler, files, respond),
        Output::Meta(form) => meta(request, *form, files, respond),
        Output::MetaField { field, form } => meta_field(request, field, *form, files, respond),
        Output::Unpack { all } => unpack(request, *all, files, respond),
        Output::Detect => respond.head(match handlers::detected_type(files) {
            Ok(media_type) => text_head(200, media_type),
            Err(message) => error_head(500, &message),
        }),
        Output::Language => language(files, respond),
    };
    if let Err(error) = answered {
        if error.kind() != io::ErrorKind::BrokenPipe {
            eprintln!("dijo: failed to answer {output:?}: {error}");
        }
    }
}

fn options(request: &ExtractRequest, handler: Option<Handler>) -> RenderOptions {
    RenderOptions {
        format: handler.map_or(Format::Text, Handler::format),
        metadata: true,
        raw_metadata: false,
        ..request.render
    }
}

fn extractor(request: &ExtractRequest, handler: Option<Handler>) -> Extractor {
    settings::extractor(
        options(request, handler),
        request.ocr,
        request.ocr_lang.as_deref(),
        request.passwords.clone(),
        request
            .deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now())),
    )
}

/// A value of each key, the only one as a string, several as an array, as Tika's JSON
/// serializer writes metadata.
fn metadata_object(metadata: &Metadata) -> Object {
    metadata
        .iter()
        .map(|(key, values)| {
            let value = match values.as_slice() {
                [value] => Value::from(value.as_str()),
                values => Value::from(values.to_vec()),
            };
            (key.clone(), value)
        })
        .collect()
}

/// Where a document was in the upload, as its object records it.
enum Place<'p> {
    /// The upload itself, which keeps the name it was given, but not dijo's for an unnamed one.
    Upload { named: bool },
    Embedded {
        path: &'p str,
        name: &'p str,
        /// None for a document a limit skipped, which was never numbered.
        id: Option<usize>,
        depth: usize,
    },
}

impl<'p> Place<'p> {
    fn of(document: &Document<'p>, named: bool) -> Self {
        match &document.source.embedded_path {
            None => Place::Upload { named },
            Some(path) => Place::Embedded {
                path,
                name: document.name,
                id: Some(document.id),
                depth: document.depth,
            },
        }
    }

    fn exception(&self) -> &'static str {
        match self {
            Place::Upload { .. } => CONTAINER_EXCEPTION,
            Place::Embedded { .. } => EMBEDDED_EXCEPTION,
        }
    }

    fn write(&self, object: &mut Object) {
        match *self {
            Place::Upload { named } => {
                if !named {
                    object.remove(RESOURCE_NAME);
                }
                object.insert(EMBEDDED_DEPTH.to_string(), Value::from("0"));
            }
            Place::Embedded {
                path,
                name,
                id,
                depth,
            } => {
                object.insert(EMBEDDED_PATH.to_string(), Value::from(format!("/{path}")));
                object.insert(RESOURCE_NAME.to_string(), Value::from(name));
                object.insert(EMBEDDED_DEPTH.to_string(), Value::from(depth.to_string()));
                if let Some(id) = id {
                    object.insert(EMBEDDED_ID.to_string(), Value::from(id.to_string()));
                }
            }
        }
    }
}

/// A document's object: its metadata, its place in the upload, its errors, and with a handler
/// its content as the handler writes it, unless it ignores it, and the handler's name.
fn record(
    open: &dyn Fn() -> Opened,
    source: &Source,
    request: &ExtractRequest,
    handler: Option<Handler>,
    place: &Place<'_>,
) -> io::Result<(Object, Outcome)> {
    let options = options(request, handler);
    let writes_content = handler.is_some_and(|handler| handler != Handler::Ignore);
    let mut content = Vec::new();
    let outcome = if writes_content {
        render::render_opened(open(), source, options, &mut content)?
    } else {
        render::render_opened(open(), source, options, &mut io::sink())?
    };
    let mut object = outcome
        .metadata
        .as_ref()
        .map(metadata_object)
        .unwrap_or_default();
    place.write(&mut object);
    if !outcome.errors.is_empty() {
        let messages: Vec<&str> = outcome
            .errors
            .iter()
            .map(|error| error.message.as_str())
            .collect();
        object.insert(
            place.exception().to_string(),
            Value::from(messages.join("\n")),
        );
    }
    if writes_content {
        object.insert(
            CONTENT.to_string(),
            Value::from(String::from_utf8_lossy(&content).into_owned()),
        );
    }
    if let Some(handler) = handler {
        object.insert(HANDLER_TYPE.to_string(), Value::from(handler.tika_name()));
    }
    Ok((object, outcome))
}

/// The record of the upload, its embedded documents' content inlined.
fn upload_record(
    request: &ExtractRequest,
    handler: Option<Handler>,
    files: &Files,
) -> io::Result<(Object, Outcome)> {
    let extractor = extractor(request, handler);
    let source = render::source(&files.path, &files.display, None);
    record(
        &|| render::open(&extractor, &files.path),
        &source,
        request,
        handler,
        &Place::Upload {
            named: is_named(files),
        },
    )
}

/// Whether the client named the upload, rather than dijo.
fn is_named(files: &Files) -> bool {
    files.display != upload::DEFAULT_NAME
}

fn report(respond: &mut dyn Respond, outcome: &Outcome, started: Instant) {
    for error in &outcome.errors {
        respond.report(Report::Failure(error.category));
    }
    respond.report(Report::Extraction {
        mime_type: outcome
            .metadata
            .as_ref()
            .and_then(schema::mime_type)
            .unwrap_or_else(|| "unknown".to_string()),
        seconds: started.elapsed().as_secs_f64(),
    });
}

fn text_head(status: u16, body: impl Into<Vec<u8>>) -> Head {
    Head {
        status,
        content_type: ContentType::Text,
        body: Some(body.into()),
    }
}

fn json_head(value: &impl Serialize) -> io::Result<Head> {
    Ok(Head {
        status: 200,
        content_type: ContentType::Json,
        body: Some(serde_json::to_vec(value)?),
    })
}

/// The head of an answer that failed for its first error, which a document that did not open
/// has.
fn failure_head(outcome: &Outcome) -> Option<Head> {
    if !matches!(outcome.status, Status::Failed | Status::Encrypted) {
        return None;
    }
    let error = outcome.errors.first()?;
    Some(error_head(status_for(error.category), &error.message))
}

fn json(
    request: &ExtractRequest,
    handler: Handler,
    files: &Files,
    respond: &mut dyn Respond,
) -> io::Result<()> {
    let started = Instant::now();
    let (object, outcome) = upload_record(request, Some(handler), files)?;
    report(respond, &outcome, started);
    respond.head(json_head(&object)?)
}

fn meta(
    request: &ExtractRequest,
    form: MetaForm,
    files: &Files,
    respond: &mut dyn Respond,
) -> io::Result<()> {
    let started = Instant::now();
    let (object, outcome) = upload_record(request, None, files)?;
    report(respond, &outcome, started);
    respond.head(match form {
        MetaForm::Json => json_head(&object)?,
        MetaForm::Csv => csv_head(object.iter()),
    })
}

fn meta_field(
    request: &ExtractRequest,
    field: &str,
    form: FieldForm,
    files: &Files,
    respond: &mut dyn Respond,
) -> io::Result<()> {
    let started = Instant::now();
    let (object, outcome) = upload_record(request, None, files)?;
    report(respond, &outcome, started);
    if let Some(head) = failure_head(&outcome) {
        return respond.head(head);
    }
    let Some(value) = object.get(field) else {
        return respond.head(error_head(
            404,
            &format!("Failed to get metadata field {field}"),
        ));
    };
    respond.head(match form {
        FieldForm::Json => json_head(&BTreeMap::from([(field, value)]))?,
        FieldForm::Csv => csv_head([(field, value)]),
        FieldForm::Plain => text_head(200, values(value).next().unwrap_or_default()),
    })
}

fn values(value: &Value) -> impl Iterator<Item = &str> {
    let values = match value {
        Value::Array(values) => values.as_slice(),
        value => std::slice::from_ref(value),
    };
    values.iter().filter_map(Value::as_str)
}

/// A line per field: its name, then its values, each a CSV field.
fn csv_head<'v>(fields: impl IntoIterator<Item = (impl AsRef<str>, &'v Value)>) -> Head {
    let mut csv = String::new();
    for (name, value) in fields {
        csv_field(&mut csv, name.as_ref());
        for value in values(value) {
            csv.push(',');
            csv_field(&mut csv, value);
        }
        csv.push('\n');
    }
    Head {
        status: 200,
        content_type: ContentType::Csv,
        body: Some(csv.into_bytes()),
    }
}

fn csv_field(csv: &mut String, field: &str) {
    if !field.contains([',', '"', '\n', '\r']) {
        csv.push_str(field);
        return;
    }
    csv.push('"');
    csv.push_str(&field.replace('"', "\"\""));
    csv.push('"');
}

fn rmeta(
    request: &ExtractRequest,
    handler: Handler,
    files: &Files,
    respond: &mut dyn Respond,
) -> io::Result<()> {
    let started = Instant::now();
    if let Err(error) = fs::create_dir(&files.scratch) {
        return respond.head(error_head(
            500,
            &format!("Failed to create {}: {error}", files.scratch.display()),
        ));
    }
    respond.head(handlers::streaming(ContentType::Json))?;
    let mut writer = BufWriter::with_capacity(BODY_CHUNK_SIZE, BodyWriter { respond });
    writer.write_all(b"[")?;
    let mut records = RmetaRecords {
        request,
        handler,
        named: is_named(files),
        sink: &mut writer,
        written: false,
    };
    let outcome = tree::render_tree(
        &extractor(request, Some(handler)),
        &files.path,
        &files.display,
        request.limits,
        &files.scratch,
        &mut records,
    )?;
    writer.write_all(b"]")?;
    writer.flush()?;
    drop(writer);
    report(respond, &outcome, started);
    Ok(())
}

/// The objects of `/rmeta`, written as the elements of one JSON array.
struct RmetaRecords<'a> {
    request: &'a ExtractRequest,
    handler: Handler,
    named: bool,
    sink: &'a mut dyn Write,
    written: bool,
}

impl RmetaRecords<'_> {
    fn write(&mut self, object: &Object) -> io::Result<()> {
        if self.written {
            self.sink.write_all(b",")?;
        }
        self.written = true;
        serde_json::to_writer(&mut *self.sink, object)?;
        Ok(())
    }
}

impl Records for RmetaRecords<'_> {
    fn document(
        &mut self,
        document: &Document<'_>,
        open: &dyn Fn() -> Opened,
    ) -> io::Result<Outcome> {
        let (object, outcome) = record(
            open,
            document.source,
            self.request,
            Some(self.handler),
            &Place::of(document, self.named),
        )?;
        self.write(&object)?;
        Ok(outcome)
    }

    fn skipped(
        &mut self,
        source: &Source,
        error: &ExtractionError,
        depth: usize,
    ) -> io::Result<()> {
        let path = source.embedded_path.as_deref().unwrap_or_default();
        let place = Place::Embedded {
            path,
            name: path.rsplit('/').next().unwrap_or(path),
            id: None,
            depth,
        };
        let mut object = Object::from([(
            place.exception().to_string(),
            Value::from(error.message.as_str()),
        )]);
        place.write(&mut object);
        self.write(&object)
    }
}

fn unpack(
    request: &ExtractRequest,
    all: bool,
    files: &Files,
    respond: &mut dyn Respond,
) -> io::Result<()> {
    if let Err(error) = fs::create_dir(&files.scratch) {
        return respond.head(error_head(
            500,
            &format!("Failed to create {}: {error}", files.scratch.display()),
        ));
    }
    let mut records = UnpackRecords {
        request,
        all,
        named: is_named(files),
        state: Unpacking::Unanswered(respond),
    };
    tree::render_tree(
        &extractor(request, None),
        &files.path,
        &files.display,
        request.limits,
        &files.scratch,
        &mut records,
    )?;
    match records.state {
        Unpacking::Unanswered(respond) => respond.head(Head {
            status: 204,
            content_type: ContentType::Text,
            body: Some(Vec::new()),
        }),
        Unpacking::Zipping(zip) => zip.finish()?.into_inner().flush(),
        Unpacking::Answered => Ok(()),
    }
}

type Archive<'a> = ZipWriter<StreamWriter<BufWriter<BodyWriter<'a>>>>;

/// How far the answer to `/unpack` is.
enum Unpacking<'a> {
    Unanswered(&'a mut dyn Respond),
    Zipping(Box<Archive<'a>>),
    /// Answered without an archive: the upload could not be read.
    Answered,
}

/// The documents of `/unpack`, written as the entries of a ZIP archive, each named by its
/// number and its type's extension.
struct UnpackRecords<'a> {
    request: &'a ExtractRequest,
    all: bool,
    named: bool,
    state: Unpacking<'a>,
}

impl<'a> UnpackRecords<'a> {
    /// The archive, its head sent first if it is not yet.
    fn archive(&mut self) -> io::Result<&mut Archive<'a>> {
        if let Unpacking::Unanswered(_) = self.state {
            let Unpacking::Unanswered(respond) =
                std::mem::replace(&mut self.state, Unpacking::Answered)
            else {
                unreachable!("the state was just matched");
            };
            respond.head(handlers::streaming(ContentType::Zip))?;
            self.state = Unpacking::Zipping(Box::new(ZipWriter::new_stream(
                BufWriter::with_capacity(BODY_CHUNK_SIZE, BodyWriter { respond }),
            )));
        }
        match &mut self.state {
            Unpacking::Zipping(zip) => Ok(zip),
            Unpacking::Unanswered(_) | Unpacking::Answered => {
                Err(io::Error::other("the answer was sent without an archive"))
            }
        }
    }

    fn entry(&mut self, document: &Document<'_>, object: &Object) -> io::Result<()> {
        let extension = object
            .get(CONTENT_TYPE)
            .and_then(Value::as_str)
            .and_then(dijo_core::media_type_extension)
            .unwrap_or(UNKNOWN_EXTENSION);
        let name = format!("{}{extension}", document.id);
        let size = fs::metadata(document.path)?.len();
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .large_file(size > u64::from(u32::MAX));
        let all = self.all;
        let zip = self.archive()?;
        zip.start_file(name.as_str(), options)?;
        io::copy(&mut File::open(document.path)?, zip)?;
        if all {
            zip.start_file(format!("{name}{METADATA_SUFFIX}"), options)?;
            serde_json::to_writer(&mut *zip, object)?;
        }
        Ok(())
    }
}

impl Records for UnpackRecords<'_> {
    fn document(
        &mut self,
        document: &Document<'_>,
        open: &dyn Fn() -> Opened,
    ) -> io::Result<Outcome> {
        let (object, mut outcome) = record(
            open,
            document.source,
            self.request,
            None,
            &Place::of(document, self.named),
        )?;
        if document.id != 0 {
            self.entry(document, &object)?;
            return Ok(outcome);
        }
        if let Some(head) = failure_head(&outcome) {
            if let Unpacking::Unanswered(respond) =
                std::mem::replace(&mut self.state, Unpacking::Answered)
            {
                respond.head(head)?;
            }
            outcome.embedded.clear();
            return Ok(outcome);
        }
        if self.all {
            self.entry(document, &object)?;
        }
        Ok(outcome)
    }

    fn skipped(
        &mut self,
        _source: &Source,
        _error: &ExtractionError,
        _depth: usize,
    ) -> io::Result<()> {
        Ok(())
    }
}

fn language(files: &Files, respond: &mut dyn Respond) -> io::Result<()> {
    let mut bytes = Vec::new();
    let read = File::open(&files.path)
        .and_then(|file| file.take(LANGUAGE_CHARS as u64 * 4).read_to_end(&mut bytes));
    if let Err(error) = read {
        return respond.head(error_head(
            500,
            &format!("Failed to read the upload: {error}"),
        ));
    }
    let text: String = String::from_utf8_lossy(&bytes)
        .chars()
        .take(LANGUAGE_CHARS)
        .collect();
    let code = language::detect(&text).map_or("", |detected| {
        iso639::two_letter(detected.code).unwrap_or(detected.code)
    });
    respond.head(text_head(200, code))
}
