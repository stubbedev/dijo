use std::os::raw::c_void;

use crate::errors::{Error, ExtractResult};
use crate::Metadata;
use jni::errors::jni_error_code_to_result;
use jni::objects::{JByteBuffer, JObject, JObjectArray, JString, JValue, JValueOwned};
use jni::{sys, JNIEnv, JavaVM};
use std::collections::HashMap;

/// Calls a static method and prints any thrown exceptions to stderr
pub fn jni_new_direct_buffer<'local>(
    env: &mut JNIEnv<'local>,
    data: *mut u8,
    len: usize,
) -> ExtractResult<JByteBuffer<'local>> {
    let direct_byte_buffer = unsafe { env.new_direct_byte_buffer(data, len) }
        .map_err(|_e| Error::JniEnvCall("Failed to create direct byte buffer"))?;

    Ok(direct_byte_buffer)
}

/// Calls a static method and prints any thrown exceptions to stderr
pub fn jni_call_static_method<'local>(
    env: &mut JNIEnv<'local>,
    class: &str,
    method: &str,
    signature: &str,
    args: &[JValue],
) -> ExtractResult<JValueOwned<'local>> {
    let call_result = env.call_static_method(class, method, signature, args);
    match call_result {
        Ok(result) => Ok(result),
        Err(error) => match error {
            jni::errors::Error::JavaException => {
                jni_check_exception(env)?;
                Err(Error::JniError(error))
            }
            _ => Err(Error::JniError(error)),
        },
    }
}

/// Calls an object method and prints any thrown exceptions to stderr
pub fn jni_call_method<'local>(
    env: &mut JNIEnv<'local>,
    obj: &JObject<'local>,
    method: &str,
    signature: &str,
    args: &[JValue],
) -> ExtractResult<JValueOwned<'local>> {
    let call_result = env.call_method(obj, method, signature, args);
    match call_result {
        Ok(result) => Ok(result),
        Err(error) => match error {
            jni::errors::Error::JavaException => {
                jni_check_exception(env)?;
                Err(Error::JniError(error))
            }
            _ => Err(Error::JniError(error)),
        },
    }
}

/// creates a new java string from a rust str
pub fn jni_new_string<'local>(env: &mut JNIEnv<'local>, s: &str) -> ExtractResult<JString<'local>> {
    match env.new_string(s) {
        Ok(s) => Ok(s),
        Err(_) => Err(Error::JniEnvCall("Couldn't create Java String")),
    }
}

/// creates a new java string from a rust str and returns it as a JValueOwned
pub fn jni_new_string_as_jvalue<'local>(
    env: &mut JNIEnv<'local>,
    s: &str,
) -> ExtractResult<JValueOwned<'local>> {
    let jstring = jni_new_string(env, s)?;
    //let jstring = env.new_string(s)?;

    Ok(JValueOwned::from(jstring))
}

/// Converts a java object to a rust string
pub fn jni_jobject_to_string<'local>(
    env: &mut JNIEnv<'local>,
    jobject: JObject<'local>,
) -> ExtractResult<String> {
    let jstring_output = JString::from(jobject);
    let javastr_output = unsafe { env.get_string_unchecked(&jstring_output)? };
    let output_str = javastr_output.to_string_lossy();
    //let output_str = javastr_output.to_str().map_err(Error::Utf8Error)?;

    Ok(output_str.to_string())
}

/// Converts a Java String[] to a Rust Vec<String>
pub fn jni_jobject_array_to_vec<'local>(
    env: &mut JNIEnv<'local>,
    array: JObject<'local>,
) -> ExtractResult<Vec<String>> {
    let j_array_string = JObjectArray::from(array);
    let j_array_length = env.get_array_length(&j_array_string)?;

    let mut vec = Vec::with_capacity(j_array_length as usize);

    for i in 0..j_array_length {
        let elem_obj = env.get_object_array_element(&j_array_string, i)?;
        let elem_str = jni_jobject_to_string(env, elem_obj)?;
        vec.push(elem_str);
    }

    Ok(vec)
}

/// Convert a Tika Metadata a Rust Metadata
pub fn jni_tika_metadata_to_rust_metadata<'local>(
    env: &mut JNIEnv<'local>,
    j_tika_metadata_object: JObject<'local>,
) -> ExtractResult<Metadata> {
    let j_keys_names = env
        .call_method(
            &j_tika_metadata_object,
            "names",
            "()[Ljava/lang/String;",
            &[],
        )?
        .l()?;
    let keys_names = jni_jobject_array_to_vec(env, j_keys_names)?;
    let mut metadata = HashMap::new();
    for key_name in keys_names.iter() {
        let j_key_name = jni_new_string_as_jvalue(env, key_name)?;
        let j_obj_array_name_metadata = env
            .call_method(
                &j_tika_metadata_object,
                "getValues",
                "(Ljava/lang/String;)[Ljava/lang/String;",
                &[(&j_key_name).into()],
            )?
            .l()?;
        let key_metadata = jni_jobject_array_to_vec(env, j_obj_array_name_metadata)?;
        metadata.insert(key_name.to_string(), key_metadata);
    }
    Ok(metadata)
}

/// Checks if there is an exception in the jni environment, describes it to
/// the stderr and finally clears it
pub fn jni_check_exception(env: &mut JNIEnv) -> ExtractResult<bool> {
    if env.exception_check()? {
        env.exception_describe()?;
        env.exception_clear()?;
        return Ok(true);
    }
    Ok(false)
}

/// Creates a new graalvm isolate using the invocation api. A [GraalVM isolate](https://medium.com/graalvm/isolates-and-compressed-references-more-flexible-and-efficient-memory-management-for-graalvm-a044cc50b67e) is a disjoint heap
/// that allows multiple tasks in the same VM instance to run independently.
///
/// This function uses the standard JVM invocation API and relies on the jni-sys crate.
/// No need to specify any libraries because the graalvm native image is already
/// linked in by the build script.
pub fn create_vm_isolate() -> JavaVM {
    let mut vm_options = [
        sys::JavaVMOption {
            optionString: c"-Djava.library.path=.".as_ptr().cast_mut(),
            extraInfo: std::ptr::null_mut(),
        },
        sys::JavaVMOption {
            optionString: c"-Djava.awt.headless=true".as_ptr().cast_mut(),
            extraInfo: std::ptr::null_mut(),
        },
    ];
    let mut args = sys::JavaVMInitArgs {
        version: sys::JNI_VERSION_1_8,
        nOptions: vm_options.len() as sys::jint,
        options: vm_options.as_mut_ptr(),
        ignoreUnrecognized: sys::JNI_FALSE,
    };
    let mut ptr: *mut sys::JavaVM = std::ptr::null_mut();
    let mut env: *mut sys::JNIEnv = std::ptr::null_mut();

    unsafe {
        let jni_res = sys::JNI_CreateJavaVM(
            &mut ptr,
            &mut env as *mut *mut sys::JNIEnv as *mut *mut c_void,
            &mut args as *mut sys::JavaVMInitArgs as *mut c_void,
        );
        jni_error_code_to_result(jni_res).unwrap_or_else(|e| {
            panic!("Failed creating the graal native vm: {:?}", e);
        });

        JavaVM::from_raw(ptr).unwrap_or_else(|e| {
            panic!("Failed creating the graal native from pointer: {:?}", e);
        })
    }
}
