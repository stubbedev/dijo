use std::env;
use std::ffi::{c_void, CString};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::OnceLock;

use flate2::read::GzDecoder;
use jni::{sys, JavaVM};
use libloading::Library;

use crate::IsolateSettings;

include!(concat!(env!("OUT_DIR"), "/native.rs"));

type CreateJavaVm =
    unsafe extern "system" fn(*mut *mut sys::JavaVM, *mut *mut c_void, *mut c_void) -> sys::jint;

/// The Tika native library, loaded once and kept loaded for the life of the process.
struct Loaded {
    dir: PathBuf,
    create_java_vm: CreateJavaVm,
    _library: Library,
}

fn loaded() -> Result<&'static Loaded, String> {
    static LOADED: OnceLock<Result<Loaded, String>> = OnceLock::new();
    LOADED.get_or_init(load).as_ref().map_err(Clone::clone)
}

/// Unpacks the embedded native libraries and loads Tika from them.
fn load() -> Result<Loaded, String> {
    let dir = install_dir()?;
    #[cfg(windows)]
    search_dependencies_in(&dir)?;
    let library_path = dir.join(TIKA_LIBRARY);
    let library = open_globally(&library_path)
        .map_err(|e| format!("Failed to load {}: {e}", library_path.display()))?;
    let create_java_vm = *unsafe { library.get::<CreateJavaVm>(b"JNI_CreateJavaVM\0") }
        .map_err(|e| format!("{} has no JNI_CreateJavaVM: {e}", library_path.display()))?;
    Ok(Loaded {
        dir,
        create_java_vm,
        _library: library,
    })
}

/// Loads the library with its symbols visible to every library loaded after it. The JDK shims
/// GraalVM emits beside it (libjvm, libjava) resolve the JNI invocation functions by name at
/// run time, and on macOS they cannot see into a library opened with RTLD_LOCAL: AWT's
/// JNI_OnLoad then spins forever in JNU_GetEnv.
#[cfg(unix)]
fn open_globally(path: &Path) -> Result<Library, libloading::Error> {
    use libloading::os::unix::{Library as UnixLibrary, RTLD_GLOBAL, RTLD_NOW};
    unsafe { UnixLibrary::open(Some(path), RTLD_NOW | RTLD_GLOBAL) }.map(Library::from)
}

#[cfg(windows)]
fn open_globally(path: &Path) -> Result<Library, libloading::Error> {
    unsafe { Library::new(path) }
}

/// Windows resolves a DLL's dependencies from the application directory, the system directories
/// and PATH, not from the DLL's own directory, so awt.dll would not find the jvm.dll and
/// java.dll shims unpacked next to it.
#[cfg(windows)]
fn search_dependencies_in(dir: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    let wide: Vec<u16> = dir
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let succeeded =
        unsafe { windows_sys::Win32::System::LibraryLoader::SetDllDirectoryW(wide.as_ptr()) };
    if succeeded == 0 {
        return Err(format!(
            "Failed to add {} to the DLL search path: {}",
            dir.display(),
            io::Error::last_os_error()
        ));
    }
    Ok(())
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

/// Creates a new GraalVM isolate: a disjoint heap, so work in one isolate cannot exhaust or
/// corrupt another. Each call to JNI_CreateJavaVM in a native image creates one.
pub(crate) fn create_isolate(settings: &IsolateSettings) -> Result<JavaVM, String> {
    let loaded = loaded()?;
    let library_dir = loaded.dir.as_path();
    let library_path = library_dir.to_str().ok_or_else(|| {
        format!(
            "The native library directory {} is not UTF-8",
            library_dir.display()
        )
    })?;
    let library_path_option = CString::new(format!("-Djava.library.path={library_path}"))
        .map_err(|e| format!("Invalid native library directory {library_path}: {e}"))?;
    let temp_dir_option = settings
        .temp_dir
        .as_deref()
        .map(|dir| {
            let dir = dir
                .to_str()
                .ok_or_else(|| format!("The temporary directory {} is not UTF-8", dir.display()))?;
            CString::new(format!("-Djava.io.tmpdir={dir}"))
                .map_err(|e| format!("Invalid temporary directory {dir}: {e}"))
        })
        .transpose()?;
    let max_heap_option = settings.max_heap_bytes.map(|bytes| {
        CString::new(format!("-XX:MaxHeapSize={bytes}")).expect("a number has no NUL")
    });
    let mut vm_options: Vec<sys::JavaVMOption> = [
        Some(library_path_option.as_c_str()),
        Some(c"-Djava.awt.headless=true"),
        temp_dir_option.as_deref(),
        max_heap_option.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(|option| sys::JavaVMOption {
        optionString: option.as_ptr().cast_mut(),
        extraInfo: std::ptr::null_mut(),
    })
    .collect();
    let mut args = sys::JavaVMInitArgs {
        version: sys::JNI_VERSION_1_8,
        nOptions: vm_options.len() as sys::jint,
        options: vm_options.as_mut_ptr(),
        ignoreUnrecognized: sys::JNI_FALSE,
    };
    let mut vm: *mut sys::JavaVM = std::ptr::null_mut();
    let mut env: *mut sys::JNIEnv = std::ptr::null_mut();

    let code = unsafe {
        (loaded.create_java_vm)(
            &mut vm,
            &mut env as *mut *mut sys::JNIEnv as *mut *mut c_void,
            &mut args as *mut sys::JavaVMInitArgs as *mut c_void,
        )
    };
    jni::errors::jni_error_code_to_result(code)
        .map_err(|e| format!("Failed to create the GraalVM isolate: {e}"))?;
    Ok(unsafe { JavaVM::from_raw(vm) })
}
