use std::env;
use std::ffi::{c_void, CString};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process;

use flate2::read::GzDecoder;
use jni::{sys, JavaVM};
use libloading::Library;

include!(concat!(env!("OUT_DIR"), "/native.rs"));

type CreateJavaVm =
    unsafe extern "system" fn(*mut *mut sys::JavaVM, *mut *mut c_void, *mut c_void) -> sys::jint;

/// The loaded Tika native library and the VM isolate created from it. The library stays
/// loaded for as long as this value lives, which is the life of the process.
pub(crate) struct Runtime {
    pub(crate) vm: JavaVM,
    _library: Library,
}

/// Unpacks the embedded native libraries, loads Tika from them and creates the VM isolate.
pub(crate) fn start() -> Result<Runtime, String> {
    let dir = install_dir()?;
    let library_path = dir.join(TIKA_LIBRARY);
    let library = unsafe { Library::new(&library_path) }
        .map_err(|e| format!("Failed to load {}: {e}", library_path.display()))?;
    let create_java_vm = *unsafe { library.get::<CreateJavaVm>(b"JNI_CreateJavaVM\0") }
        .map_err(|e| format!("{} has no JNI_CreateJavaVM: {e}", library_path.display()))?;
    let vm = create_vm_isolate(create_java_vm, &dir)?;
    Ok(Runtime {
        vm,
        _library: library,
    })
}

fn cache_dir() -> Result<PathBuf, String> {
    if let Some(dir) = env::var_os("XDG_CACHE_HOME").filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let (variable, subdir) = if cfg!(windows) {
        ("LOCALAPPDATA", "")
    } else if cfg!(target_os = "macos") {
        ("HOME", "Library/Caches")
    } else {
        ("HOME", ".cache")
    };
    env::var_os(variable)
        .filter(|dir| !dir.is_empty())
        .map(|dir| PathBuf::from(dir).join(subdir))
        .ok_or_else(|| {
            format!(
                "Cannot unpack the Tika native libraries: neither XDG_CACHE_HOME nor {variable} \
                 is set"
            )
        })
}

fn install_dir() -> Result<PathBuf, String> {
    let root = cache_dir()?.join("extractous");
    let dir = root.join(NATIVE_FINGERPRINT);
    if dir.is_dir() {
        return Ok(dir);
    }
    fs::create_dir_all(&root).map_err(|e| format!("Failed to create {}: {e}", root.display()))?;
    let staging = root.join(format!(".{NATIVE_FINGERPRINT}.{}", process::id()));
    unpack(&staging).map_err(|e| format!("Failed to unpack into {}: {e}", staging.display()))?;
    match fs::rename(&staging, &dir) {
        Ok(()) => Ok(dir),
        Err(_) if dir.is_dir() => {
            fs::remove_dir_all(&staging)
                .map_err(|e| format!("Failed to remove {}: {e}", staging.display()))?;
            Ok(dir)
        }
        Err(e) => Err(format!(
            "Failed to move {} to {}: {e}",
            staging.display(),
            dir.display()
        )),
    }
}

fn unpack(staging: &Path) -> io::Result<()> {
    match fs::remove_dir_all(staging) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    fs::create_dir(staging)?;
    for (name, compressed) in &NATIVE_LIBRARIES {
        let mut content = Vec::new();
        GzDecoder::new(*compressed).read_to_end(&mut content)?;
        fs::write(staging.join(name), content)?;
    }
    Ok(())
}

/// Creates a new graalvm isolate using the invocation api. A [GraalVM isolate](https://medium.com/graalvm/isolates-and-compressed-references-more-flexible-and-efficient-memory-management-for-graalvm-a044cc50b67e) is a disjoint heap
/// that allows multiple tasks in the same VM instance to run independently.
fn create_vm_isolate(create_java_vm: CreateJavaVm, library_dir: &Path) -> Result<JavaVM, String> {
    let library_path = library_dir.to_str().ok_or_else(|| {
        format!(
            "The native library directory {} is not UTF-8",
            library_dir.display()
        )
    })?;
    let library_path_option = CString::new(format!("-Djava.library.path={library_path}"))
        .map_err(|e| format!("Invalid native library directory {library_path}: {e}"))?;
    let mut vm_options = [
        sys::JavaVMOption {
            optionString: library_path_option.as_ptr().cast_mut(),
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
    let mut vm: *mut sys::JavaVM = std::ptr::null_mut();
    let mut env: *mut sys::JNIEnv = std::ptr::null_mut();

    let code = unsafe {
        create_java_vm(
            &mut vm,
            &mut env as *mut *mut sys::JNIEnv as *mut *mut c_void,
            &mut args as *mut sys::JavaVMInitArgs as *mut c_void,
        )
    };
    jni::errors::jni_error_code_to_result(code)
        .map_err(|e| format!("Failed to create the GraalVM isolate: {e}"))?;
    unsafe { JavaVM::from_raw(vm) }.map_err(|e| format!("Failed to wrap the GraalVM isolate: {e}"))
}
