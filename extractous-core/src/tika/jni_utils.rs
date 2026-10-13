use crate::errors::{Error, ExtractResult};
use crate::Metadata;
use bytemuck::cast_slice_mut;
use jni::objects::{JByteArray, JByteBuffer, JObject, JObjectArray, JString, JValue, JValueOwned};
use jni::signature::RuntimeMethodSignature;
use jni::strings::JNIString;
use jni::Env;
use std::collections::HashMap;

const STRING: &str = "Ljava/lang/String;";
const STRING_ARRAY: &str = "[Ljava/lang/String;";
const BYTE_ARRAY: &str = "[B";

/// The signature of a method taking `params` and returning `ret`, both as JNI type descriptors
fn signature(params: &str, ret: &str) -> ExtractResult<RuntimeMethodSignature> {
    Ok(RuntimeMethodSignature::from_str(format!(
        "({params}){ret}"
    ))?)
}

/// Wraps memory owned by Rust in a Java direct byte buffer without copying it
pub fn jni_new_direct_buffer<'local>(
    env: &mut Env<'local>,
    data: *mut u8,
    len: usize,
) -> ExtractResult<JByteBuffer<'local>> {
    unsafe { env.new_direct_byte_buffer(data, len) }
        .map_err(|_e| Error::JniEnvCall("Failed to create direct byte buffer"))
}

/// Turns a thrown Java exception into [`Error::JavaException`]
fn checked<'local>(
    env: &mut Env<'local>,
    result: jni::errors::Result<JValueOwned<'local>>,
) -> ExtractResult<JValueOwned<'local>> {
    match result {
        Ok(result) => Ok(result),
        Err(jni::errors::Error::JavaException) => Err(take_java_exception(env)),
        Err(error) => Err(Error::JniError(error)),
    }
}

/// Calls a static method of the class with the JNI name `class`, taking `params` and returning
/// `ret`, turning a thrown Java exception into [`Error::JavaException`]
pub fn jni_call_static_method<'local>(
    env: &mut Env<'local>,
    class: &str,
    method: &str,
    params: &str,
    ret: &str,
    args: &[JValue],
) -> ExtractResult<JValueOwned<'local>> {
    let class = env.find_class(JNIString::new(class))?;
    let signature = signature(params, ret)?;
    let result = env.call_static_method(
        &class,
        JNIString::new(method),
        signature.method_signature(),
        args,
    );
    checked(env, result)
}

/// Calls an object method taking `params` and returning `ret`, turning a thrown Java exception
/// into [`Error::JavaException`]
pub fn jni_call_method<'local>(
    env: &mut Env<'local>,
    obj: &JObject,
    method: &str,
    params: &str,
    ret: &str,
    args: &[JValue],
) -> ExtractResult<JValueOwned<'local>> {
    let signature = signature(params, ret)?;
    let result = env.call_method(
        obj,
        JNIString::new(method),
        signature.method_signature(),
        args,
    );
    checked(env, result)
}

/// Calls an object method that returns a `String`, which is None when it returns null
pub fn jni_call_string_method(
    env: &mut Env<'_>,
    obj: &JObject,
    method: &str,
    params: &str,
    args: &[JValue],
) -> ExtractResult<Option<String>> {
    let value = jni_call_method(env, obj, method, params, STRING, args)?;
    returned_string(env, value)
}

/// The text of the `String` a method declared to return one returned, None for null
fn returned_string(env: &Env<'_>, value: JValueOwned) -> ExtractResult<Option<String>> {
    let value = value.l()?;
    // Safety: the only callers pass the result of a call whose signature returns a String.
    let string = unsafe { JString::from_raw(env, value.into_raw()) };
    jni_string(env, &string)
}

/// Calls an object method that returns a `String[]`
pub fn jni_call_string_array_method(
    env: &mut Env<'_>,
    obj: &JObject,
    method: &str,
    params: &str,
    args: &[JValue],
) -> ExtractResult<Vec<String>> {
    let value = jni_call_method(env, obj, method, params, STRING_ARRAY, args)?.l()?;
    // Safety: the method's signature declares that it returns a String[].
    let array = unsafe { JObjectArray::<JString>::from_raw(env, value.into_raw()) };
    let length = array.len(env)?;
    let mut strings = Vec::with_capacity(length);
    for index in 0..length {
        let element = array.get_element(env, index)?;
        strings.push(
            jni_string(env, &element)?.ok_or_else(|| {
                Error::Unknown(format!("{method} returned an array holding null"))
            })?,
        );
    }
    Ok(strings)
}

/// Calls an object method that returns an array of objects
pub fn jni_call_object_array_method<'local>(
    env: &mut Env<'local>,
    obj: &JObject,
    method: &str,
    element: &str,
) -> ExtractResult<JObjectArray<'local>> {
    let value = jni_call_method(env, obj, method, "", &format!("[{element}"), &[])?.l()?;
    // Safety: the method's signature declares that it returns an array of objects.
    Ok(unsafe { JObjectArray::<JObject>::from_raw(env, value.into_raw()) })
}

/// Calls an object method that returns a `byte[]` and copies its bytes
pub fn jni_call_byte_array_method(
    env: &mut Env<'_>,
    obj: &JObject,
    method: &str,
) -> ExtractResult<Vec<u8>> {
    let value = jni_call_method(env, obj, method, "", BYTE_ARRAY, &[])?.l()?;
    // Safety: the method's signature declares that it returns a byte[].
    let array = unsafe { JByteArray::from_raw(env, value.into_raw()) };
    let mut content = vec![0u8; array.len(env)?];
    array.get_region(env, 0, cast_slice_mut(&mut content))?;
    Ok(content)
}

/// Creates a new Java string from a Rust str
pub fn jni_new_string<'local>(env: &mut Env<'local>, s: &str) -> ExtractResult<JString<'local>> {
    env.new_string(s)
        .map_err(|_| Error::JniEnvCall("Couldn't create Java String"))
}

/// The text of a Java string, None for null
fn jni_string(env: &Env<'_>, string: &JString) -> ExtractResult<Option<String>> {
    if string.is_null() {
        return Ok(None);
    }
    Ok(Some(string.try_to_string(env)?))
}

/// Convert a Tika Metadata a Rust Metadata
pub fn jni_tika_metadata_to_rust_metadata(
    env: &mut Env<'_>,
    j_tika_metadata_object: &JObject,
) -> ExtractResult<Metadata> {
    let keys_names = jni_call_string_array_method(env, j_tika_metadata_object, "names", "", &[])?;
    let mut metadata = HashMap::with_capacity(keys_names.len());
    for key_name in keys_names {
        let j_key_name = jni_new_string(env, &key_name)?;
        let values = jni_call_string_array_method(
            env,
            j_tika_metadata_object,
            "getValues",
            STRING,
            &[JValue::Object(&j_key_name)],
        )?;
        metadata.insert(key_name, values);
    }
    Ok(metadata)
}

/// Clears the pending Java exception and turns it into an [`Error::JavaException`] that
/// carries the class and message of the exception and of every cause behind it.
pub fn take_java_exception(env: &mut Env) -> Error {
    let Some(throwable) = env.exception_occurred() else {
        return Error::JniEnvCall("A Java call failed without throwing an exception");
    };
    env.exception_clear();
    let describe = |env: &mut Env| -> ExtractResult<String> {
        let description = jni_call_static_method(
            env,
            "ai/yobix/Throwables",
            "describe",
            "Ljava/lang/Throwable;",
            STRING,
            &[JValue::Object(&throwable)],
        )?;
        returned_string(env, description)?
            .ok_or_else(|| Error::Unknown("Throwables.describe returned null".to_string()))
    };
    match describe(env) {
        Ok(description) => Error::JavaException(description),
        Err(error) => error,
    }
}
