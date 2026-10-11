use std::sync::OnceLock;

use crate::config::ParserConfigs;
use crate::embedded::EmbeddedExtractResult;
use crate::errors::{Error, ExtractResult};
use crate::tika::jni_utils::*;
use crate::tika::native;
use crate::tika::wrappers::*;
use crate::{CharSet, Metadata, StreamReader};
use jni::objects::{JObject, JValue};
use jni::{AttachGuard, JNIEnv, JavaVM};

const TIKA_NATIVE_MAIN: &str = "ai/yobix/TikaNativeMain";
const CONFIGS_SIGNATURE: &str = "Lorg/apache/tika/parser/pdf/PDFParserConfig;\
                                 Lorg/apache/tika/parser/microsoft/OfficeParserConfig;\
                                 Lorg/apache/tika/parser/ocr/TesseractOCRConfig;";

/// Returns the VM isolate shared by every extraction in the process, starting it on first use.
pub(crate) fn vm() -> ExtractResult<&'static JavaVM> {
    static RUNTIME: OnceLock<Result<native::Runtime, String>> = OnceLock::new();
    match RUNTIME.get_or_init(native::start) {
        Ok(runtime) => Ok(&runtime.vm),
        Err(message) => Err(Error::NativeLibrary(message.clone())),
    }
}

fn attach_current_thread() -> ExtractResult<AttachGuard<'static>> {
    Ok(vm()?.attach_current_thread()?)
}

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

    fn into_java<'local>(self, env: &mut JNIEnv<'local>) -> ExtractResult<JObject<'local>> {
        match self {
            Source::File(text) | Source::Url(text) => Ok(jni_new_string(env, text)?.into()),
            Source::Bytes(bytes) => {
                Ok(jni_new_direct_buffer(env, bytes.as_ptr().cast_mut(), bytes.len())?.into())
            }
        }
    }
}

fn call_tika<'local>(
    env: &mut JNIEnv<'local>,
    source: Source,
    method: &str,
    configs: &ParserConfigs,
    tail_signature: &str,
    tail_args: &[JValue],
) -> ExtractResult<JObject<'local>> {
    let source_obj = source.into_java(env)?;
    let [pdf, office, ocr] = java_parser_configs(env, configs)?;
    let signature = format!("({}{CONFIGS_SIGNATURE}{tail_signature}", source.java_type());
    let mut args = Vec::with_capacity(4 + tail_args.len());
    args.extend([
        JValue::Object(&source_obj),
        JValue::Object(&pdf),
        JValue::Object(&office),
        JValue::Object(&ocr),
    ]);
    args.extend_from_slice(tail_args);
    Ok(jni_call_static_method(env, TIKA_NATIVE_MAIN, method, &signature, &args)?.l()?)
}

/// Parses a document into a stream of its text, decoded with `char_set`.
pub(crate) fn parse_to_stream(
    source: Source,
    char_set: &CharSet,
    configs: &ParserConfigs,
    as_xml: bool,
) -> ExtractResult<(StreamReader, Metadata)> {
    let mut env = attach_current_thread()?;
    let char_set_obj: JObject = jni_new_string(&mut env, &char_set.to_string())?.into();
    let result = call_tika(
        &mut env,
        source,
        &format!("parse{}", source.method_suffix()),
        configs,
        "Ljava/lang/String;Z)Lai/yobix/ReaderResult;",
        &[JValue::Object(&char_set_obj), JValue::Bool(as_xml.into())],
    )?;
    let (reader, metadata) = reader_result(&mut env, result)?;
    let inner = JReaderInputStream::new(env, reader)?;
    Ok((StreamReader { inner }, metadata))
}

/// Parses a document into a string of at most `max_length` characters.
pub(crate) fn parse_to_string(
    source: Source,
    max_length: i32,
    configs: &ParserConfigs,
    as_xml: bool,
) -> ExtractResult<(String, Metadata)> {
    let mut env = attach_current_thread()?;
    let result = call_tika(
        &mut env,
        source,
        &format!("parse{}ToString", source.method_suffix()),
        configs,
        "IZ)Lai/yobix/StringResult;",
        &[JValue::Int(max_length), JValue::Bool(as_xml.into())],
    )?;
    string_result(&mut env, result)
}

/// Extracts the documents embedded in a document, such as images and attachments.
pub(crate) fn extract_embedded(
    source: Source,
    configs: &ParserConfigs,
) -> ExtractResult<EmbeddedExtractResult> {
    let mut env = attach_current_thread()?;
    let result = call_tika(
        &mut env,
        source,
        &format!("extractEmbedded{}", source.method_suffix()),
        configs,
        ")Lai/yobix/EmbeddedExtractResult;",
        &[],
    )?;
    embedded_result(&mut env, result)
}

/// Detects the media type of a document with Tika's default detector.
pub(crate) fn detect(source: Source) -> ExtractResult<String> {
    let mut env = attach_current_thread()?;
    let source_obj = source.into_java(&mut env)?;
    let signature = format!("({})Lai/yobix/StringResult;", source.java_type());
    let result = jni_call_static_method(
        &mut env,
        TIKA_NATIVE_MAIN,
        &format!("detect{}", source.method_suffix()),
        &signature,
        &[JValue::Object(&source_obj)],
    )?
    .l()?;
    let (media_type, _) = string_result(&mut env, result)?;
    Ok(media_type)
}
