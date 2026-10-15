use std::time::Duration;

use crate::config::{ParserConfigs, Password};
use crate::embedded::{EmbeddedExtractResult, EmbeddedSpool};
use crate::errors::{Error, ExtractResult};
use crate::extractor::Stream;
use crate::isolate::Isolate;
use crate::tika::jni_utils::*;
use crate::tika::wrappers::*;
use crate::{CharSet, Metadata, StreamReader};
use jni::objects::{JObject, JValue};
use jni::Env;

const TIKA_NATIVE_MAIN: &str = "ai/yobix/TikaNativeMain";
const CONFIGS_SIGNATURE: &str = "Lorg/apache/tika/parser/pdf/PDFParserConfig;\
                                 Lorg/apache/tika/parser/microsoft/OfficeParserConfig;\
                                 Lorg/apache/tika/parser/ocr/TesseractOCRConfig;\
                                 Ljava/lang/String;";

/// Where a document is read from.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Source<'a> {
    File(&'a str),
    Bytes(&'a [u8]),
    Url(&'a str),
}

impl Source<'_> {
    fn method_suffix(&self) -> &'static str {
        match self {
            Source::File(_) => "File",
            Source::Bytes(_) => "Bytes",
            Source::Url(_) => "Url",
        }
    }

    fn java_type(&self) -> &'static str {
        match self {
            Source::File(_) | Source::Url(_) => "Ljava/lang/String;",
            Source::Bytes(_) => "Ljava/nio/ByteBuffer;",
        }
    }

    fn into_java<'local>(self, env: &mut Env<'local>) -> ExtractResult<JObject<'local>> {
        match self {
            Source::File(text) | Source::Url(text) => Ok(jni_new_string(env, text)?.into()),
            Source::Bytes(bytes) => {
                Ok(jni_new_direct_buffer(env, bytes.as_ptr().cast_mut(), bytes.len())?.into())
            }
        }
    }
}

/// A `TikaNativeMain` method that takes a source, the parser configurations and a password, then
/// `params`, and returns an instance of `returns`.
struct TikaMethod<'a> {
    name: String,
    params: &'a str,
    returns: &'a str,
}

fn call_tika<'local>(
    env: &mut Env<'local>,
    source: Source,
    method: TikaMethod,
    configs: &ParserConfigs,
    password: Option<&Password>,
    tail_args: &[JValue],
) -> ExtractResult<JObject<'local>> {
    let source_obj = source.into_java(env)?;
    let [pdf, office, ocr] = java_parser_configs(env, configs)?;
    let password_obj: JObject = match password {
        Some(password) => jni_new_string(env, password.expose())?.into(),
        None => JObject::null(),
    };
    let params = format!("{}{CONFIGS_SIGNATURE}{}", source.java_type(), method.params);
    let mut args = Vec::with_capacity(5 + tail_args.len());
    args.extend([
        JValue::Object(&source_obj),
        JValue::Object(&pdf),
        JValue::Object(&office),
        JValue::Object(&ocr),
        JValue::Object(&password_obj),
    ]);
    args.extend_from_slice(tail_args);
    Ok(jni_call_static_method(
        env,
        TIKA_NATIVE_MAIN,
        &method.name,
        &params,
        method.returns,
        &args,
    )?
    .l()?)
}

/// The timeout in milliseconds as Java takes it: 0 for none, and at least 1 for any limit.
fn timeout_millis(timeout: Option<Duration>) -> i64 {
    timeout.map_or(0, |timeout| {
        i64::try_from(timeout.as_millis())
            .unwrap_or(i64::MAX)
            .max(1)
    })
}

/// Parses a document into a stream of its text, decoded with `char_set`.
/// How a streamed extraction is encoded, bounded and treats embedded documents.
pub(crate) struct StreamOptions<'a> {
    pub(crate) char_set: CharSet,
    pub(crate) as_xml: bool,
    pub(crate) timeout: Option<Duration>,
    pub(crate) embedded: Option<&'a EmbeddedSpool>,
}

/// Parses a document into a stream of its text.
pub(crate) fn parse_to_stream(
    isolate: &'static Isolate,
    source: Source,
    configs: &ParserConfigs,
    password: Option<&Password>,
    options: &StreamOptions,
) -> ExtractResult<(StreamReader, Metadata)> {
    let (stream, metadata) = isolate.with_env(|env| {
        let char_set_obj: JObject = jni_new_string(env, &options.char_set.to_string())?.into();
        let spool_dir_obj: JObject = match options.embedded {
            Some(spool) => {
                let dir = spool.dir.to_str().ok_or_else(|| {
                    Error::IoError(format!(
                        "The embedded document directory {} is not UTF-8",
                        spool.dir.display()
                    ))
                })?;
                jni_new_string(env, dir)?.into()
            }
            None => JObject::null(),
        };
        let byte_budget = options.embedded.map_or(0, |spool| {
            i64::try_from(spool.byte_budget).unwrap_or(i64::MAX)
        });
        let result = call_tika(
            env,
            source,
            TikaMethod {
                name: format!("parse{}", source.method_suffix()),
                params: "Ljava/lang/String;ZJLjava/lang/String;J",
                returns: "Lai/yobix/ReaderResult;",
            },
            configs,
            password,
            &[
                JValue::Object(&char_set_obj),
                JValue::Bool(options.as_xml),
                JValue::Long(timeout_millis(options.timeout)),
                JValue::Object(&spool_dir_obj),
                JValue::Long(byte_budget),
            ],
        )?;
        let (stream, metadata) = reader_result(env, &result)?;
        Ok::<_, Error>((env.new_global_ref(stream)?, metadata))
    })?;
    let inner = JInputStream::new(isolate, stream);
    Ok((
        StreamReader {
            inner: Stream::Tika(inner),
        },
        metadata,
    ))
}

/// Parses a document into a string of at most `max_length` characters.
pub(crate) fn parse_to_string(
    isolate: &Isolate,
    source: Source,
    max_length: i32,
    configs: &ParserConfigs,
    password: Option<&Password>,
    as_xml: bool,
) -> ExtractResult<(String, Metadata)> {
    isolate.with_env(|env| {
        let result = call_tika(
            env,
            source,
            TikaMethod {
                name: format!("parse{}ToString", source.method_suffix()),
                params: "IZ",
                returns: "Lai/yobix/StringResult;",
            },
            configs,
            password,
            &[JValue::Int(max_length), JValue::Bool(as_xml)],
        )?;
        string_result(env, &result)
    })
}

/// Extracts the documents embedded in a document, such as images and attachments.
pub(crate) fn extract_embedded(
    isolate: &Isolate,
    source: Source,
    configs: &ParserConfigs,
    password: Option<&Password>,
) -> ExtractResult<EmbeddedExtractResult> {
    isolate.with_env(|env| {
        let result = call_tika(
            env,
            source,
            TikaMethod {
                name: format!("extractEmbedded{}", source.method_suffix()),
                params: "",
                returns: "Lai/yobix/EmbeddedExtractResult;",
            },
            configs,
            password,
            &[],
        )?;
        embedded_result(env, &result)
    })
}

/// Detects the media type of a document with Tika's default detector.
pub(crate) fn detect(isolate: &Isolate, source: Source) -> ExtractResult<String> {
    detect_with_metadata(isolate, source).map(|(media_type, _)| media_type)
}

/// Detects the media type of a document with Tika's default detector, with the metadata opening
/// the input and detecting it set: the resource name, the length and what the detectors record.
pub(crate) fn detect_with_metadata(
    isolate: &Isolate,
    source: Source,
) -> ExtractResult<(String, Metadata)> {
    isolate.with_env(|env| {
        let source_obj = source.into_java(env)?;
        let result = jni_call_static_method(
            env,
            TIKA_NATIVE_MAIN,
            &format!("detect{}", source.method_suffix()),
            source.java_type(),
            "Lai/yobix/StringResult;",
            &[JValue::Object(&source_obj)],
        )?
        .l()?;
        string_result(env, &result)
    })
}

/// Parses a document embedded in one a native parser reads, as Tika parses an embedded document:
/// with the embedding extraction's settings, given the metadata the embedding parser gives it as
/// names with their values, in order, which go to Java as `EventRecorder` encodes strings.
pub(crate) fn parse_embedded(
    isolate: &Isolate,
    data: &[u8],
    metadata: &[(&str, &str)],
    configs: &ParserConfigs,
    password: Option<&Password>,
) -> ExtractResult<EmbeddedEvents> {
    let mut pairs = Vec::new();
    for (name, value) in metadata {
        for text in [name, value] {
            let length = i32::try_from(text.len()).map_err(|_| {
                Error::ParseError(format!(
                    "An embedded document's metadata holds {} bytes, more than Java can",
                    text.len()
                ))
            })?;
            pairs.extend_from_slice(&length.to_be_bytes());
            pairs.extend_from_slice(text.as_bytes());
        }
    }
    isolate.with_env(|env| {
        let pairs = env.byte_array_from_slice(&pairs)?;
        let result = call_tika(
            env,
            Source::Bytes(data),
            TikaMethod {
                name: "parseEmbeddedBytes".to_owned(),
                params: "[B",
                returns: "Lai/yobix/EmbeddedEvents;",
            },
            configs,
            password,
            &[JValue::Object(&pairs)],
        )?;
        embedded_events(env, &result)
    })
}

/// Waits for every parse in the isolate to end, cancelled ones included, for at most `timeout`.
/// Returns whether they all did.
pub(crate) fn await_idle_parses(isolate: &Isolate, timeout: Duration) -> ExtractResult<bool> {
    isolate.with_env(|env| {
        let millis = i64::try_from(timeout.as_millis()).unwrap_or(i64::MAX);
        Ok(jni_call_static_method(
            env,
            TIKA_NATIVE_MAIN,
            "awaitIdleParses",
            "J",
            "Z",
            &[JValue::Long(millis)],
        )?
        .z()?)
    })
}
