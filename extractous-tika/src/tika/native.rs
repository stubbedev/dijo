//! Tika's GraalVM native image as the process runs it: loaded as a shared library, or the static
//! image this crate is linked into.

use crate::bundle::{unpacked, OCR_FILES, OCR_HEIF_DECODER, OCR_MODELS};
use crate::isolate::IsolateSettings;

#[cfg(not(feature = "static-image"))]
pub(crate) use shared_library::create_isolate;
#[cfg(feature = "static-image")]
pub(crate) use static_image::create_isolate;

/// The system properties Tika is started with: its temporary directory, headless AWT, the
/// processors OCR may use, the bundled tesseract's language data, the bundled tesseract library
/// unless the static build links it, and the bundled heif-png.
fn system_properties(settings: &IsolateSettings) -> Result<Vec<(&'static str, String)>, String> {
    let mut properties = vec![("java.awt.headless", "true".to_string())];
    if let Some(dir) = &settings.temp_dir {
        let dir = dir
            .to_str()
            .ok_or_else(|| format!("The temporary directory {} is not UTF-8", dir.display()))?;
        properties.push(("java.io.tmpdir", dir.to_string()));
    }
    if let Some(processors) = settings.processors {
        properties.push(("extractous.processors", processors.to_string()));
    }
    if !OCR_FILES.is_empty() {
        let dir = unpacked()?;
        #[cfg(not(feature = "static"))]
        if let Some(library) = crate::bundle::OCR_TESSERACT_LIBRARY {
            properties.push((
                "extractous.ocr.library",
                dir.join(library).display().to_string(),
            ));
        }
        properties.push((
            "extractous.ocr.tessdata",
            dir.join(OCR_MODELS).display().to_string(),
        ));
        if let Some(decoder) = OCR_HEIF_DECODER {
            properties.push((
                "extractous.ocr.heif",
                dir.join(decoder).display().to_string(),
            ));
        }
    }
    Ok(properties)
}

#[cfg(feature = "static-image")]
mod static_image {
    use jni::objects::JValue;
    use jni::{sys, JavaVM};

    use super::system_properties;
    use crate::errors::Error;
    use crate::isolate::IsolateSettings;
    use crate::tika::jni_utils::{jni_call_static_method, jni_new_string};

    const STATIC_IMAGE: &str = "ai/yobix/StaticImage";

    unsafe extern "system" {
        fn JNI_GetCreatedJavaVMs(
            vms: *mut *mut sys::JavaVM,
            size: sys::jsize,
            count: *mut sys::jsize,
        ) -> sys::jint;
    }

    /// The Java VM of the static image this crate is linked into, which started before the
    /// program did, set up with what `create_isolate` of a shared library build passes as
    /// options.
    pub(crate) fn create_isolate(settings: &IsolateSettings) -> Result<JavaVM, String> {
        let mut vm: *mut sys::JavaVM = std::ptr::null_mut();
        let mut count: sys::jsize = 0;
        let code = unsafe { JNI_GetCreatedJavaVMs(&mut vm, 1, &mut count) };
        jni::errors::jni_error_code_to_result(code)
            .map_err(|e| format!("Failed to find the Java VM of the static image: {e}"))?;
        if count != 1 || vm.is_null() {
            return Err(format!(
                "The static image runs {count} Java VMs, expected one"
            ));
        }
        let vm = unsafe { JavaVM::from_raw(vm) };
        let properties = system_properties(settings)?;
        let max_heap_bytes = settings.max_heap_bytes.map_or(0, |bytes| bytes.get());
        let min_heap_bytes = settings.min_heap_bytes();
        vm.attach_current_thread_for_scope(|env| {
            for (key, value) in &properties {
                let key = jni_new_string(env, key)?;
                let value = jni_new_string(env, value)?;
                jni_call_static_method(
                    env,
                    STATIC_IMAGE,
                    "setProperty",
                    "Ljava/lang/String;Ljava/lang/String;",
                    "V",
                    &[JValue::Object(&key), JValue::Object(&value)],
                )?;
            }
            if max_heap_bytes > 0 {
                jni_call_static_method(
                    env,
                    STATIC_IMAGE,
                    "setMaxHeapBytes",
                    "J",
                    "V",
                    &[JValue::Long(max_heap_bytes as i64)],
                )?;
            }
            jni_call_static_method(
                env,
                STATIC_IMAGE,
                "setMinHeapBytes",
                "J",
                "V",
                &[JValue::Long(min_heap_bytes as i64)],
            )?;
            Ok::<_, Error>(())
        })
        .map_err(|e| format!("Failed to configure the Java VM of the static image: {e}"))?;
        Ok(vm)
    }
}

#[cfg(not(feature = "static-image"))]
mod shared_library {
    use std::ffi::{c_void, CString};
    #[cfg(windows)]
    use std::io;
    use std::path::Path;
    use std::sync::OnceLock;

    use jni::{sys, JavaVM};
    use libloading::Library;

    use super::system_properties;
    use crate::bundle::{unpacked, TIKA_LIBRARY};
    use crate::isolate::IsolateSettings;

    type CreateJavaVm = unsafe extern "system" fn(
        *mut *mut sys::JavaVM,
        *mut *mut c_void,
        *mut c_void,
    ) -> sys::jint;

    /// The Tika native library, loaded once and kept loaded for the life of the process.
    struct Loaded {
        dir: &'static Path,
        create_java_vm: CreateJavaVm,
        _library: Library,
    }

    fn loaded() -> Result<&'static Loaded, String> {
        static LOADED: OnceLock<Result<Loaded, String>> = OnceLock::new();
        LOADED.get_or_init(load).as_ref().map_err(Clone::clone)
    }

    /// Unpacks the embedded native libraries and loads Tika from them.
    fn load() -> Result<Loaded, String> {
        let dir = unpacked()?;
        #[cfg(windows)]
        search_dependencies_in(dir)?;
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

    /// Creates a new GraalVM isolate: a disjoint heap, so work in one isolate cannot exhaust or
    /// corrupt another. Each call to JNI_CreateJavaVM in a native image creates one.
    pub(crate) fn create_isolate(settings: &IsolateSettings) -> Result<JavaVM, String> {
        let loaded = loaded()?;
        let library_dir = loaded.dir;
        let library_path = library_dir.to_str().ok_or_else(|| {
            format!(
                "The native library directory {} is not UTF-8",
                library_dir.display()
            )
        })?;
        let library_path_option = CString::new(format!("-Djava.library.path={library_path}"))
            .map_err(|e| format!("Invalid native library directory {library_path}: {e}"))?;
        let max_heap_option = settings.max_heap_bytes.map(|bytes| {
            CString::new(format!("-XX:MaxHeapSize={bytes}")).expect("a number has no NUL")
        });
        let min_heap_option =
            CString::new(format!("-XX:MinHeapSize={}", settings.min_heap_bytes()))
                .expect("a number has no NUL");
        let property_options = system_properties(settings)?
            .into_iter()
            .map(|(key, value)| {
                CString::new(format!("-D{key}={value}"))
                    .map_err(|e| format!("Invalid value {value:?} of {key}: {e}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut vm_options: Vec<sys::JavaVMOption> = [
            Some(library_path_option.as_c_str()),
            max_heap_option.as_deref(),
            Some(min_heap_option.as_c_str()),
        ]
        .into_iter()
        .flatten()
        .chain(property_options.iter().map(CString::as_c_str))
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
}
