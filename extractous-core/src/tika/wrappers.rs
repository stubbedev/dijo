use crate::config::{JavaValue, ParserConfigs};
use crate::embedded::{EmbeddedDocument, EmbeddedExtractResult, SpooledEmbedded};
use crate::errors::{Error, ExtractResult};
use crate::tika::jni_utils::{
    jni_call_byte_array_method, jni_call_method, jni_call_object_array_method,
    jni_call_string_method, jni_new_direct_buffer, jni_new_string,
    jni_tika_metadata_to_rust_metadata,
};
use crate::{Metadata, OfficeParserConfig, PdfParserConfig, TesseractOcrConfig};
use jni::objects::{Global, JObject, JValue};
use jni::strings::JNIString;
use jni::Env;
use std::path::PathBuf;

use crate::isolate::Isolate;

/// What `ParsingInputStream.readInto` returns when its deadline passes before more text.
const TIMED_OUT: i32 = -2;

/// Wrapper for the `ai.yobix.ParsingInputStream` that yields the extracted text. Each read hands
/// Java the caller's buffer as a direct `ByteBuffer`, so the text is copied once, straight into it.
pub struct JInputStream {
    internal: Global<JObject<'static>>,
    finished: bool,
    isolate: &'static Isolate,
}

impl JInputStream {
    pub(crate) fn new(isolate: &'static Isolate, internal: Global<JObject<'static>>) -> Self {
        Self {
            internal,
            finished: false,
            isolate,
        }
    }

    pub(crate) fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        Ok(self.read_into(buf)?)
    }

    fn read_into(&mut self, buf: &mut [u8]) -> ExtractResult<usize> {
        let read = self.isolate.with_env(|env| self.read_in_frame(env, buf))?;
        if read == 0 {
            self.finished = true;
        }
        Ok(read)
    }

    pub(crate) fn metadata(&self) -> ExtractResult<Metadata> {
        if !self.finished {
            return Err(Error::StreamNotFinished);
        }
        self.isolate
            .with_env(|env| java_result_metadata(env, self.internal.as_obj()))
    }

    pub(crate) fn embedded(&self) -> ExtractResult<Vec<SpooledEmbedded>> {
        if !self.finished {
            return Err(Error::StreamNotFinished);
        }
        self.isolate.with_env(|env| {
            let array = jni_call_object_array_method(
                env,
                self.internal.as_obj(),
                "getEmbedded",
                "Lai/yobix/SpooledEmbedded;",
            )?;
            (0..array.len(env)?)
                .map(|index| {
                    let item = array.get_element(env, index)?;
                    spooled_embedded(env, &item)
                })
                .collect()
        })
    }

    fn read_in_frame(&self, env: &mut Env<'_>, buf: &mut [u8]) -> ExtractResult<usize> {
        let target = jni_new_direct_buffer(env, buf.as_mut_ptr(), buf.len())?;
        let read = jni_call_method(
            env,
            self.internal.as_obj(),
            "readInto",
            "Ljava/nio/ByteBuffer;",
            "I",
            &[JValue::Object(&target)],
        )?
        .i()?;
        match read {
            TIMED_OUT => Err(Error::Timeout),
            read if read < 0 => Ok(0),
            read => Ok(read as usize),
        }
    }
}

impl Drop for JInputStream {
    fn drop(&mut self) {
        self.isolate
            .with_env(|env| {
                jni_call_method(env, self.internal.as_obj(), "close", "", "V", &[])?;
                Ok(())
            })
            .ok();
    }
}

fn java_result_error(env: &mut Env<'_>, result: &JObject) -> ExtractResult<Option<Error>> {
    if !jni_call_method(env, result, "isError", "", "Z", &[])?.z()? {
        return Ok(None);
    }
    let status = jni_call_method(env, result, "getStatus", "", "B", &[])?.b()?;
    let message = required_string(env, result, "getErrorMessage")?;
    Ok(Some(match status {
        1 => Error::IoError(message),
        2 => Error::ParseError(message),
        3 => Error::Encrypted(message),
        _ => Error::Unknown(message),
    }))
}

fn java_result_metadata(env: &mut Env<'_>, result: &JObject) -> ExtractResult<Metadata> {
    let metadata_obj = jni_call_method(
        env,
        result,
        "getMetadata",
        "",
        "Lorg/apache/tika/metadata/Metadata;",
        &[],
    )?
    .l()?;
    jni_tika_metadata_to_rust_metadata(env, &metadata_obj)
}

/// Converts an `ai.yobix.StringResult` into the extracted text and its metadata.
pub(crate) fn string_result(
    env: &mut Env<'_>,
    result: &JObject,
) -> ExtractResult<(String, Metadata)> {
    if let Some(error) = java_result_error(env, result)? {
        return Err(error);
    }
    let content = required_string(env, result, "getContent")?;
    Ok((content, java_result_metadata(env, result)?))
}

/// Converts an `ai.yobix.ReaderResult` into the `java.io.InputStream` of the encoded text and
/// the metadata.
pub(crate) fn reader_result<'local>(
    env: &mut Env<'local>,
    result: &JObject,
) -> ExtractResult<(JObject<'local>, Metadata)> {
    if let Some(error) = java_result_error(env, result)? {
        return Err(error);
    }
    let reader =
        jni_call_method(env, result, "getStream", "", "Ljava/io/InputStream;", &[])?.l()?;
    Ok((reader, java_result_metadata(env, result)?))
}

/// What Tika made of a document embedded in one a native parser reads: the SAX events of its
/// body as `ai.yobix.EventRecorder` encodes them, and how its parse ended.
pub(crate) struct EmbeddedEvents {
    pub(crate) events: Vec<u8>,
    pub(crate) outcome: EmbeddedOutcome,
}

/// How the parse of an embedded document ended.
pub(crate) enum EmbeddedOutcome {
    /// It ended, with the document's metadata and, if its parser failed in a way Tika records
    /// in the embedding document's metadata, that failure as Tika formats it there.
    Parsed {
        metadata: Metadata,
        recorded_exception: Option<String>,
    },
    /// It failed in a way that fails the embedding document too, after writing the events.
    Failed(Error),
}

/// Converts an `ai.yobix.EmbeddedEvents` into [`EmbeddedEvents`].
pub(crate) fn embedded_events(
    env: &mut Env<'_>,
    result: &JObject,
) -> ExtractResult<EmbeddedEvents> {
    let events = jni_call_byte_array_method(env, result, "getEvents")?;
    let outcome = match java_result_error(env, result)? {
        Some(error) => EmbeddedOutcome::Failed(error),
        None => EmbeddedOutcome::Parsed {
            metadata: java_result_metadata(env, result)?,
            recorded_exception: optional_string(env, result, "getRecordedException")?,
        },
    };
    Ok(EmbeddedEvents { events, outcome })
}

/// Converts an `ai.yobix.EmbeddedExtractResult` into an [`EmbeddedExtractResult`].
pub(crate) fn embedded_result(
    env: &mut Env<'_>,
    result: &JObject,
) -> ExtractResult<EmbeddedExtractResult> {
    if let Some(error) = java_result_error(env, result)? {
        return Err(error);
    }
    let documents_obj = jni_call_method(
        env,
        result,
        "getEmbeddedDocuments",
        "",
        "Ljava/util/List;",
        &[],
    )?
    .l()?;
    let size = jni_call_method(env, &documents_obj, "size", "", "I", &[])?.i()?;
    let documents = (0..size)
        .map(|index| {
            let document = jni_call_method(
                env,
                &documents_obj,
                "get",
                "I",
                "Ljava/lang/Object;",
                &[JValue::Int(index)],
            )?
            .l()?;
            embedded_document(env, &document)
        })
        .collect::<ExtractResult<_>>()?;
    Ok(EmbeddedExtractResult {
        documents,
        metadata: java_result_metadata(env, result)?,
    })
}

fn spooled_embedded(env: &mut Env<'_>, item: &JObject) -> ExtractResult<SpooledEmbedded> {
    Ok(SpooledEmbedded {
        name: required_string(env, item, "getName")?,
        content_type: optional_string(env, item, "getContentType")?,
        path: PathBuf::from(required_string(env, item, "getPath")?),
        size: u64::try_from(jni_call_method(env, item, "getSize", "", "J", &[])?.j()?)
            .map_err(|_| Error::Unknown("SpooledEmbedded.getSize was negative".to_string()))?,
        truncated: jni_call_method(env, item, "isTruncated", "", "Z", &[])?.z()?,
    })
}

fn optional_string(
    env: &mut Env<'_>,
    obj: &JObject,
    getter: &str,
) -> ExtractResult<Option<String>> {
    jni_call_string_method(env, obj, getter, "", &[])
}

fn required_string(env: &mut Env<'_>, obj: &JObject, getter: &str) -> ExtractResult<String> {
    optional_string(env, obj, getter)?.ok_or_else(|| {
        Error::Unknown(format!(
            "{getter} returned null for a value that is always set"
        ))
    })
}

fn embedded_document(env: &mut Env<'_>, document: &JObject) -> ExtractResult<EmbeddedDocument> {
    Ok(EmbeddedDocument {
        resource_name: required_string(env, document, "getResourceName")?,
        content_type: required_string(env, document, "getContentType")?,
        content: jni_call_byte_array_method(env, document, "getContent")?,
        embedded_relationship_id: optional_string(env, document, "getEmbeddedRelationshipId")?,
    })
}

fn new_java_config<'local>(
    env: &mut Env<'local>,
    class: &str,
    settings: impl Iterator<Item = (&'static str, JavaValue)>,
) -> ExtractResult<JObject<'local>> {
    let class = env.find_class(JNIString::new(class))?;
    let obj = env.new_object(&class, jni::jni_sig!("()V"), &[])?;
    for (setter, value) in settings {
        match value {
            JavaValue::Bool(val) => {
                jni_call_method(env, &obj, setter, "Z", "V", &[JValue::from(val)])?
            }
            JavaValue::Int(val) => {
                jni_call_method(env, &obj, setter, "I", "V", &[JValue::from(val)])?
            }
            JavaValue::Str(val) => {
                let string = jni_new_string(env, &val)?;
                jni_call_method(
                    env,
                    &obj,
                    setter,
                    "Ljava/lang/String;",
                    "V",
                    &[JValue::Object(&string)],
                )?
            }
        };
    }
    Ok(obj)
}

/// Creates the Java PDF, Office and Tesseract configurations, in that order.
pub(crate) fn java_parser_configs<'local>(
    env: &mut Env<'local>,
    configs: &ParserConfigs,
) -> ExtractResult<[JObject<'local>; 3]> {
    Ok([
        new_java_config(
            env,
            PdfParserConfig::JAVA_CLASS,
            configs.pdf.java_settings(),
        )?,
        new_java_config(
            env,
            OfficeParserConfig::JAVA_CLASS,
            configs.office.java_settings(),
        )?,
        new_java_config(
            env,
            TesseractOcrConfig::JAVA_CLASS,
            configs.ocr.java_settings(),
        )?,
    ])
}
