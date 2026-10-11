use crate::config::{JavaValue, ParserConfigs};
use crate::embedded::{EmbeddedDocument, EmbeddedExtractResult};
use crate::errors::{Error, ExtractResult};
use crate::tika::jni_utils::{
    jni_call_method, jni_jobject_to_string, jni_new_direct_buffer, jni_new_string_as_jvalue,
    jni_tika_metadata_to_rust_metadata,
};
use crate::tika::vm;
use crate::{Metadata, OfficeParserConfig, PdfParserConfig, TesseractOcrConfig};
use bytemuck::cast_slice_mut;
use jni::objects::{GlobalRef, JByteArray, JObject, JValue};
use jni::{AttachGuard, JNIEnv};

/// Wrapper for the `ai.yobix.ParsingInputStream` that yields the extracted text. Each read hands
/// Java the caller's buffer as a direct `ByteBuffer`, so the text is copied once, straight into it.
pub struct JInputStream {
    internal: GlobalRef,
    finished: bool,
    #[cfg(feature = "stream-attachguard")]
    _guard: AttachGuard<'static>,
}

impl JInputStream {
    pub(crate) fn new(guard: AttachGuard<'static>, internal: GlobalRef) -> Self {
        #[cfg(not(feature = "stream-attachguard"))]
        drop(guard);
        Self {
            internal,
            finished: false,
            #[cfg(feature = "stream-attachguard")]
            _guard: guard,
        }
    }

    pub(crate) fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        Ok(self.read_into(buf)?)
    }

    fn read_into(&mut self, buf: &mut [u8]) -> ExtractResult<usize> {
        let read = vm()?
            .attach_current_thread()?
            .with_local_frame(4, |env| self.read_in_frame(env, buf))?;
        if read == 0 {
            self.finished = true;
        }
        Ok(read)
    }

    pub(crate) fn metadata(&self) -> ExtractResult<Metadata> {
        if !self.finished {
            return Err(Error::StreamNotFinished);
        }
        vm()?
            .attach_current_thread()?
            .with_local_frame(8, |env| java_result_metadata(env, self.internal.as_obj()))
    }

    fn read_in_frame(&self, env: &mut JNIEnv<'_>, buf: &mut [u8]) -> ExtractResult<usize> {
        let target = jni_new_direct_buffer(env, buf.as_mut_ptr(), buf.len())?;
        let read = jni_call_method(
            env,
            &self.internal,
            "readInto",
            "(Ljava/nio/ByteBuffer;)I",
            &[JValue::Object(&target)],
        )?
        .i()?;
        if read < 0 {
            return Ok(0);
        }
        Ok(read as usize)
    }
}

impl Drop for JInputStream {
    fn drop(&mut self) {
        if let Ok(mut env) = vm().and_then(|vm| Ok(vm.attach_current_thread()?)) {
            env.with_local_frame(2, |env| -> ExtractResult<()> {
                jni_call_method(env, &self.internal, "close", "()V", &[])?;
                Ok(())
            })
            .ok();
        }
    }
}

fn java_result_error<'local>(
    env: &mut JNIEnv<'local>,
    result: &JObject<'local>,
) -> ExtractResult<Option<Error>> {
    if !jni_call_method(env, result, "isError", "()Z", &[])?.z()? {
        return Ok(None);
    }
    let status = jni_call_method(env, result, "getStatus", "()B", &[])?.b()?;
    let message_obj =
        jni_call_method(env, result, "getErrorMessage", "()Ljava/lang/String;", &[])?.l()?;
    let message = jni_jobject_to_string(env, message_obj)?;
    Ok(Some(match status {
        1 => Error::IoError(message),
        2 => Error::ParseError(message),
        3 => Error::Encrypted(message),
        _ => Error::Unknown(message),
    }))
}

fn java_result_metadata<'local>(
    env: &mut JNIEnv<'local>,
    result: &JObject<'local>,
) -> ExtractResult<Metadata> {
    let metadata_obj = jni_call_method(
        env,
        result,
        "getMetadata",
        "()Lorg/apache/tika/metadata/Metadata;",
        &[],
    )?
    .l()?;
    jni_tika_metadata_to_rust_metadata(env, metadata_obj)
}

/// Converts an `ai.yobix.StringResult` into the extracted text and its metadata.
pub(crate) fn string_result<'local>(
    env: &mut JNIEnv<'local>,
    result: JObject<'local>,
) -> ExtractResult<(String, Metadata)> {
    if let Some(error) = java_result_error(env, &result)? {
        return Err(error);
    }
    let content_obj =
        jni_call_method(env, &result, "getContent", "()Ljava/lang/String;", &[])?.l()?;
    let content = jni_jobject_to_string(env, content_obj)?;
    Ok((content, java_result_metadata(env, &result)?))
}

/// Converts an `ai.yobix.ReaderResult` into the `java.io.InputStream` of the encoded text and
/// the metadata.
pub(crate) fn reader_result<'local>(
    env: &mut JNIEnv<'local>,
    result: JObject<'local>,
) -> ExtractResult<(JObject<'local>, Metadata)> {
    if let Some(error) = java_result_error(env, &result)? {
        return Err(error);
    }
    let reader = jni_call_method(env, &result, "getStream", "()Ljava/io/InputStream;", &[])?.l()?;
    Ok((reader, java_result_metadata(env, &result)?))
}

/// Converts an `ai.yobix.EmbeddedExtractResult` into an [`EmbeddedExtractResult`].
pub(crate) fn embedded_result<'local>(
    env: &mut JNIEnv<'local>,
    result: JObject<'local>,
) -> ExtractResult<EmbeddedExtractResult> {
    if let Some(error) = java_result_error(env, &result)? {
        return Err(error);
    }
    let documents_obj = jni_call_method(
        env,
        &result,
        "getEmbeddedDocuments",
        "()Ljava/util/List;",
        &[],
    )?
    .l()?;
    let size = jni_call_method(env, &documents_obj, "size", "()I", &[])?.i()?;
    let documents = (0..size)
        .map(|index| {
            let document = jni_call_method(
                env,
                &documents_obj,
                "get",
                "(I)Ljava/lang/Object;",
                &[JValue::Int(index)],
            )?
            .l()?;
            embedded_document(env, document)
        })
        .collect::<ExtractResult<_>>()?;
    Ok(EmbeddedExtractResult {
        documents,
        metadata: java_result_metadata(env, &result)?,
    })
}

fn optional_string<'local>(
    env: &mut JNIEnv<'local>,
    obj: &JObject<'local>,
    getter: &str,
) -> ExtractResult<Option<String>> {
    let value = jni_call_method(env, obj, getter, "()Ljava/lang/String;", &[])?.l()?;
    if value.is_null() {
        return Ok(None);
    }
    jni_jobject_to_string(env, value).map(Some)
}

fn required_string<'local>(
    env: &mut JNIEnv<'local>,
    obj: &JObject<'local>,
    getter: &str,
) -> ExtractResult<String> {
    optional_string(env, obj, getter)?.ok_or_else(|| {
        Error::Unknown(format!(
            "EmbeddedDocument.{getter} returned null for a value Tika always sets"
        ))
    })
}

fn embedded_document<'local>(
    env: &mut JNIEnv<'local>,
    document: JObject<'local>,
) -> ExtractResult<EmbeddedDocument> {
    let content_obj =
        JByteArray::from(jni_call_method(env, &document, "getContent", "()[B", &[])?.l()?);
    let mut content = vec![0u8; env.get_array_length(&content_obj)? as usize];
    env.get_byte_array_region(&content_obj, 0, cast_slice_mut(&mut content))?;
    Ok(EmbeddedDocument {
        resource_name: required_string(env, &document, "getResourceName")?,
        content_type: required_string(env, &document, "getContentType")?,
        content,
        embedded_relationship_id: optional_string(env, &document, "getEmbeddedRelationshipId")?,
    })
}

fn new_java_config<'local>(
    env: &mut JNIEnv<'local>,
    class: &str,
    settings: impl Iterator<Item = (&'static str, JavaValue)>,
) -> ExtractResult<JObject<'local>> {
    let obj = env.new_object(class, "()V", &[])?;
    for (setter, value) in settings {
        match value {
            JavaValue::Bool(val) => {
                jni_call_method(env, &obj, setter, "(Z)V", &[JValue::from(val)])?
            }
            JavaValue::Int(val) => {
                jni_call_method(env, &obj, setter, "(I)V", &[JValue::from(val)])?
            }
            JavaValue::Str(val) => {
                let string = jni_new_string_as_jvalue(env, &val)?;
                jni_call_method(
                    env,
                    &obj,
                    setter,
                    "(Ljava/lang/String;)V",
                    &[(&string).into()],
                )?
            }
        };
    }
    Ok(obj)
}

/// Creates the Java PDF, Office and Tesseract configurations, in that order.
pub(crate) fn java_parser_configs<'local>(
    env: &mut JNIEnv<'local>,
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
