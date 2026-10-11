use crate::config::{JavaValue, ParserConfigs};
use crate::embedded::{EmbeddedDocument, EmbeddedExtractResult};
use crate::errors::{Error, ExtractResult};
use crate::tika::jni_utils::{
    jni_call_method, jni_jobject_to_string, jni_new_string_as_jvalue,
    jni_tika_metadata_to_rust_metadata,
};
use crate::tika::vm;
use crate::{Metadata, OfficeParserConfig, PdfParserConfig, TesseractOcrConfig, DEFAULT_BUF_SIZE};
use bytemuck::cast_slice_mut;
use jni::objects::{GlobalRef, JByteArray, JObject, JValue};
use jni::sys::jsize;
use jni::{AttachGuard, JNIEnv};

/// Optimized wrapper for [`JObject`]s that contain `org.apache.commons.io.input.ReaderInputStream`
/// It saves a GlobalRef to the java object, which is cleared when the last GlobalRef is dropped
/// Implements [`Drop] trait to properly close the `org.apache.commons.io.input.ReaderInputStream`
///
/// Performance optimizations:
/// - Larger default buffer size for better throughput
/// - Adaptive buffer sizing based on read patterns
/// - Reduced JNI calls through buffer reuse
pub struct JReaderInputStream {
    internal: GlobalRef,
    buffer: GlobalRef,
    capacity: jsize,
    #[cfg(feature = "stream-attachguard")]
    _guard: AttachGuard<'static>,
    // Track read patterns for adaptive buffer sizing
    total_reads: usize,
    large_reads: usize,
}

impl JReaderInputStream {
    pub(crate) fn new(guard: AttachGuard<'static>, obj: JObject<'_>) -> ExtractResult<Self> {
        // Creates new jbyte array
        let capacity = DEFAULT_BUF_SIZE as jsize;
        let jbyte_array = guard.new_byte_array(capacity)?;

        Ok(Self {
            internal: guard.new_global_ref(obj)?,
            buffer: guard.new_global_ref(jbyte_array)?,
            capacity,
            #[cfg(feature = "stream-attachguard")]
            _guard: guard,
            total_reads: 0,
            large_reads: 0,
        })
    }

    pub(crate) fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut env = vm().attach_current_thread().map_err(Error::JniError)?;

        let length = buf.len() as jsize;

        // Track read patterns for adaptive buffer sizing
        self.total_reads += 1;
        if length > DEFAULT_BUF_SIZE as jsize {
            self.large_reads += 1;
        }

        // More aggressive adaptive buffer sizing for better performance
        let optimal_capacity = if self.total_reads > 5 && self.large_reads > self.total_reads / 3 {
            // More than 33% of reads are large, use 3x buffer size for better throughput
            (length * 3).max(DEFAULT_BUF_SIZE as jsize * 2)
        } else if self.total_reads > 20 {
            // After many reads, use at least 2x default buffer size
            length.max(DEFAULT_BUF_SIZE as jsize * 2)
        } else {
            length.max(self.capacity)
        };

        if optimal_capacity > self.capacity {
            // Create the new byte array with the optimal capacity
            let jbyte_array = env
                .new_byte_array(optimal_capacity)
                .map_err(|_e| Error::JniEnvCall("Failed to create byte array"))?;

            self.buffer = env
                .new_global_ref(jbyte_array)
                .map_err(|_e| Error::JniEnvCall("Failed to create global reference"))?;

            self.capacity = optimal_capacity;
        }

        // // Create the java byte array
        // let length = buf.len() as jsize;
        // let jbyte_array = env
        //     .new_byte_array(length)
        //     .map_err(|_e| Error::JniEnvCall("Failed to create byte array"))?;

        // Call the Java Reader's `read` method
        let call_result = jni_call_method(
            &mut env,
            &self.internal,
            "read",
            "([BII)I",
            &[
                JValue::Object(&self.buffer),
                JValue::Int(0),
                JValue::Int(length),
            ],
        );
        let num_read_bytes = call_result?.i().map_err(Error::JniError)?;

        // Get self.buffer object as a local reference
        let obj_local = env
            .new_local_ref(&self.buffer)
            .map_err(|_e| Error::JniEnvCall("Failed to create local ref"))?;

        // cast because java byte array is i8[]
        let buf_of_i8: &mut [i8] = cast_slice_mut(buf);

        // Get the bytes from the Java byte array to the Rust byte array
        // This is a copy or just memory reference. POTENTIAL performance improvement
        env.get_byte_array_region(JByteArray::from(obj_local), 0, buf_of_i8)
            .map_err(|_e| Error::JniEnvCall("Failed to get byte array region"))?;

        if num_read_bytes == -1 {
            // End of stream reached
            Ok(0)
        } else {
            Ok(num_read_bytes as usize)
        }
    }
}

impl Drop for JReaderInputStream {
    fn drop(&mut self) {
        if let Ok(mut env) = vm().attach_current_thread() {
            // Call the Java Reader's `close` method
            jni_call_method(&mut env, &self.internal, "close", "()V", &[]).ok();
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

/// Converts an `ai.yobix.ReaderResult` into its
/// `org.apache.commons.io.input.ReaderInputStream` and the metadata.
pub(crate) fn reader_result<'local>(
    env: &mut JNIEnv<'local>,
    result: JObject<'local>,
) -> ExtractResult<(JObject<'local>, Metadata)> {
    if let Some(error) = java_result_error(env, &result)? {
        return Err(error);
    }
    let reader = jni_call_method(
        env,
        &result,
        "getReader",
        "()Lorg/apache/commons/io/input/ReaderInputStream;",
        &[],
    )?
    .l()?;
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
