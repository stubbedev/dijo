use std::sync::Arc;

use crate::config::{ParserConfigs, Password};
use crate::embedded::EmbeddedExtractResult;
use crate::errors::{Error, ExtractResult};
use crate::isolate::Isolate;
use crate::tika::jni_utils::*;
use crate::tika::wrappers::*;
use crate::{CharSet, Metadata, StreamReader};
use jni::objects::{JObject, JValue};
use jni::JNIEnv;

const TIKA_NATIVE_MAIN: &str = "ai/yobix/TikaNativeMain";
const LOCAL_FRAME_CAPACITY: i32 = 32;
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
    password: Option<&Password>,
    tail_signature: &str,
    tail_args: &[JValue],
) -> ExtractResult<JObject<'local>> {
    let source_obj = source.into_java(env)?;
    let [pdf, office, ocr] = java_parser_configs(env, configs)?;
    let password_obj: JObject = match password {
        Some(password) => jni_new_string(env, password.as_str())?.into(),
        None => JObject::null(),
    };
    let signature = format!("({}{CONFIGS_SIGNATURE}{tail_signature}", source.java_type());
    let mut args = Vec::with_capacity(5 + tail_args.len());
    args.extend([
        JValue::Object(&source_obj),
        JValue::Object(&pdf),
        JValue::Object(&office),
        JValue::Object(&ocr),
        JValue::Object(&password_obj),
    ]);
    args.extend_from_slice(tail_args);
    Ok(jni_call_static_method(env, TIKA_NATIVE_MAIN, method, &signature, &args)?.l()?)
}

fn in_local_frame<T>(
    isolate: &Isolate,
    f: impl FnOnce(&mut JNIEnv<'_>) -> ExtractResult<T>,
) -> ExtractResult<T> {
    isolate.attach()?.with_local_frame(LOCAL_FRAME_CAPACITY, f)
}

/// Parses a document into a stream of its text, decoded with `char_set`.
pub(crate) fn parse_to_stream(
    isolate: &Arc<Isolate>,
    source: Source,
    char_set: &CharSet,
    configs: &ParserConfigs,
    password: Option<&Password>,
    as_xml: bool,
) -> ExtractResult<(StreamReader, Metadata)> {
    let (stream, metadata) = in_local_frame(isolate, |env| {
        let char_set_obj: JObject = jni_new_string(env, &char_set.to_string())?.into();
        let result = call_tika(
            env,
            source,
            &format!("parse{}", source.method_suffix()),
            configs,
            password,
            "Ljava/lang/String;Z)Lai/yobix/ReaderResult;",
            &[JValue::Object(&char_set_obj), JValue::Bool(as_xml.into())],
        )?;
        let (stream, metadata) = reader_result(env, result)?;
        Ok::<_, Error>((env.new_global_ref(stream)?, metadata))
    })?;
    let inner = JInputStream::new(Arc::clone(isolate), stream);
    Ok((StreamReader { inner }, metadata))
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
    in_local_frame(isolate, |env| {
        let result = call_tika(
            env,
            source,
            &format!("parse{}ToString", source.method_suffix()),
            configs,
            password,
            "IZ)Lai/yobix/StringResult;",
            &[JValue::Int(max_length), JValue::Bool(as_xml.into())],
        )?;
        string_result(env, result)
    })
}

/// Extracts the documents embedded in a document, such as images and attachments.
pub(crate) fn extract_embedded(
    isolate: &Isolate,
    source: Source,
    configs: &ParserConfigs,
    password: Option<&Password>,
) -> ExtractResult<EmbeddedExtractResult> {
    in_local_frame(isolate, |env| {
        let result = call_tika(
            env,
            source,
            &format!("extractEmbedded{}", source.method_suffix()),
            configs,
            password,
            ")Lai/yobix/EmbeddedExtractResult;",
            &[],
        )?;
        embedded_result(env, result)
    })
}

/// Detects the media type of a document with Tika's default detector.
pub(crate) fn detect(isolate: &Isolate, source: Source) -> ExtractResult<String> {
    in_local_frame(isolate, |env| {
        let source_obj = source.into_java(env)?;
        let signature = format!("({})Lai/yobix/StringResult;", source.java_type());
        let result = jni_call_static_method(
            env,
            TIKA_NATIVE_MAIN,
            &format!("detect{}", source.method_suffix()),
            &signature,
            &[JValue::Object(&source_obj)],
        )?
        .l()?;
        let (media_type, _) = string_result(env, result)?;
        Ok(media_type)
    })
}
