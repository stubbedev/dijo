use crate::config::JavaValue;
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

/// Wrapper for the Java class  `ai.yobix.StringResult`
/// Upon creation it parses the java StringResult object and saves the converted Rust string
pub struct JStringResult {
    pub content: String,
    pub metadata: Metadata,
}

impl<'local> JStringResult {
    pub(crate) fn new(env: &mut JNIEnv<'local>, obj: JObject<'local>) -> ExtractResult<Self> {
        let is_error = jni_call_method(env, &obj, "isError", "()Z", &[])?.z()?;

        if is_error {
            let status = jni_call_method(env, &obj, "getStatus", "()B", &[])?.b()?;
            let msg_obj = env
                .call_method(&obj, "getErrorMessage", "()Ljava/lang/String;", &[])?
                .l()?;
            let msg = jni_jobject_to_string(env, msg_obj)?;
            match status {
                1 => Err(Error::IoError(msg)),
                2 => Err(Error::ParseError(msg)),
                _ => Err(Error::Unknown(msg)),
            }
        } else {
            let call_result_obj = env
                .call_method(&obj, "getContent", "()Ljava/lang/String;", &[])?
                .l()?;
            let content = jni_jobject_to_string(env, call_result_obj)?;
            let tika_metadata_obj: JObject = env
                .call_method(
                    &obj,
                    "getMetadata",
                    "()Lorg/apache/tika/metadata/Metadata;",
                    &[],
                )?
                .l()?;
            let metadata = jni_tika_metadata_to_rust_metadata(env, tika_metadata_obj)?;
            Ok(Self { content, metadata })
        }
    }
}

/// Wrapper for the Java class  `ai.yobix.ReaderResult`
/// Upon creation it parses the java ReaderResult object and saves the java
/// `org.apache.commons.io.input.ReaderInputStream` object, which later can be used for reading
pub struct JReaderResult<'local> {
    pub java_reader: JObject<'local>,
    pub metadata: Metadata,
}

impl<'local> JReaderResult<'local> {
    pub(crate) fn new(env: &mut JNIEnv<'local>, obj: JObject<'local>) -> ExtractResult<Self> {
        let is_error = jni_call_method(env, &obj, "isError", "()Z", &[])?.z()?;

        if is_error {
            let status = jni_call_method(env, &obj, "getStatus", "()B", &[])?.b()?;
            let msg_obj = env
                .call_method(&obj, "getErrorMessage", "()Ljava/lang/String;", &[])?
                .l()?;
            let msg = jni_jobject_to_string(env, msg_obj)?;
            match status {
                1 => Err(Error::IoError(msg)),
                2 => Err(Error::ParseError(msg)),
                _ => Err(Error::Unknown(msg)),
            }
        } else {
            let reader_obj = jni_call_method(
                env,
                &obj,
                "getReader",
                "()Lorg/apache/commons/io/input/ReaderInputStream;",
                &[],
            )?
            .l()?;

            let tika_metadata_obj: JObject = env
                .call_method(
                    &obj,
                    "getMetadata",
                    "()Lorg/apache/tika/metadata/Metadata;",
                    &[],
                )?
                .l()?;
            let metadata = jni_tika_metadata_to_rust_metadata(env, tika_metadata_obj)?;

            Ok(Self {
                java_reader: reader_obj,
                metadata,
            })
        }
    }
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
    pdf_conf: &PdfParserConfig,
    office_conf: &OfficeParserConfig,
    ocr_conf: &TesseractOcrConfig,
) -> ExtractResult<[JObject<'local>; 3]> {
    Ok([
        new_java_config(env, PdfParserConfig::JAVA_CLASS, pdf_conf.java_settings())?,
        new_java_config(
            env,
            OfficeParserConfig::JAVA_CLASS,
            office_conf.java_settings(),
        )?,
        new_java_config(
            env,
            TesseractOcrConfig::JAVA_CLASS,
            ocr_conf.java_settings(),
        )?,
    ])
}

/// Wrapper for [`JObject`]s that contain `ai.yobix.EmbeddedExtractResult`.
pub(crate) struct JEmbeddedExtractResult {
    pub(crate) error_code: u8,
    pub(crate) error_message: Option<String>,
    pub(crate) documents: Vec<JEmbeddedDocument>,
    pub(crate) metadata: Metadata,
}

impl JEmbeddedExtractResult {
    pub(crate) fn new<'local>(
        env: &mut JNIEnv<'local>,
        obj: JObject<'local>,
    ) -> ExtractResult<Self> {
        // Get error code
        let error_code = jni_call_method(env, &obj, "getErrorCode", "()B", &[])?
            .b()
            .unwrap_or(0) as u8;

        // Get error message if error
        let error_message = if error_code != 0 {
            let msg_obj =
                jni_call_method(env, &obj, "getErrorMessage", "()Ljava/lang/String;", &[])?.l()?;
            if !msg_obj.is_null() {
                Some(jni_jobject_to_string(env, msg_obj)?)
            } else {
                None
            }
        } else {
            None
        };

        // Get embedded documents list
        let docs_list =
            jni_call_method(env, &obj, "getEmbeddedDocuments", "()Ljava/util/List;", &[])?.l()?;

        // Convert Java List to Vec
        let size = jni_call_method(env, &docs_list, "size", "()I", &[])?.i()? as usize;
        let mut documents = Vec::with_capacity(size);

        for i in 0..size {
            let doc_obj = jni_call_method(
                env,
                &docs_list,
                "get",
                "(I)Ljava/lang/Object;",
                &[JValue::from(i as i32)],
            )?
            .l()?;
            documents.push(JEmbeddedDocument::new(env, doc_obj)?);
        }

        // For now, we'll create empty metadata
        // TODO: Get metadata from parent document if needed
        let metadata = Metadata::new();

        Ok(Self {
            error_code,
            error_message,
            documents,
            metadata,
        })
    }
}

/// Wrapper for `ai.yobix.EmbeddedExtractResult$EmbeddedDocument`
pub(crate) struct JEmbeddedDocument {
    pub(crate) resource_name: String,
    pub(crate) content_type: String,
    pub(crate) content: Vec<u8>,
    pub(crate) embedded_relationship_id: Option<String>,
}

impl JEmbeddedDocument {
    pub(crate) fn new<'local>(
        env: &mut JNIEnv<'local>,
        obj: JObject<'local>,
    ) -> ExtractResult<Self> {
        // Get resource name
        let name_obj =
            jni_call_method(env, &obj, "getResourceName", "()Ljava/lang/String;", &[])?.l()?;
        let resource_name = jni_jobject_to_string(env, name_obj)?;

        // Get content type
        let type_obj =
            jni_call_method(env, &obj, "getContentType", "()Ljava/lang/String;", &[])?.l()?;
        let content_type = jni_jobject_to_string(env, type_obj)?;

        // Get content bytes
        let content_array = jni_call_method(env, &obj, "getContent", "()[B", &[])?.l()?;
        let content = if !content_array.is_null() {
            let array = JByteArray::from(content_array);
            let len = env.get_array_length(&array)?;
            let mut content = vec![0u8; len as usize];
            env.get_byte_array_region(&array, 0, cast_slice_mut(&mut content))?;
            content
        } else {
            Vec::new()
        };

        // Get embedded relationship id
        let rel_obj = jni_call_method(
            env,
            &obj,
            "getEmbeddedRelationshipId",
            "()Ljava/lang/String;",
            &[],
        )?
        .l()?;
        let embedded_relationship_id = if !rel_obj.is_null() {
            Some(jni_jobject_to_string(env, rel_obj)?)
        } else {
            None
        };

        Ok(Self {
            resource_name,
            content_type,
            content,
            embedded_relationship_id,
        })
    }
}
