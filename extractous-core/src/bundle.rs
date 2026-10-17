//! The files the build embedded in the program: the OCR bundle (`EXTRACTOUS_OCR_BUNDLE`), and in a
//! build with Tika that loads it as a shared library, Tika's native libraries. They are unpacked
//! once into a cache directory named after them.

use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::OnceLock;

use flate2::read::GzDecoder;

include!(concat!(env!("OUT_DIR"), "/native.rs"));

/// The embedded files, unpacked once into a cache directory named after them.
pub(crate) fn unpacked() -> Result<&'static Path, String> {
    static UNPACKED: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    UNPACKED
        .get_or_init(install_dir)
        .as_ref()
        .map(PathBuf::as_path)
        .map_err(Clone::clone)
}

/// Every embedded file with its path below the unpacked directory.
fn embedded_files() -> impl Iterator<Item = &'static (&'static str, &'static [u8])> {
    #[cfg(all(feature = "tika", not(feature = "static-image")))]
    let files = NATIVE_LIBRARIES.iter().chain(&OCR_FILES);
    #[cfg(not(all(feature = "tika", not(feature = "static-image"))))]
    let files = OCR_FILES.iter();
    files
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
                "Cannot unpack the embedded native files: neither XDG_CACHE_HOME nor {variable} \
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
    for (name, compressed) in embedded_files() {
        let mut content = Vec::new();
        GzDecoder::new(*compressed).read_to_end(&mut content)?;
        let path = staging.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, content)?;
        if is_program(name) {
            make_executable(&path)?;
        }
    }
    Ok(())
}

/// Whether an embedded file is a program the build runs: the bundled heif-png.
fn is_program(name: &str) -> bool {
    OCR_HEIF_DECODER == Some(name)
}

#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_: &Path) -> io::Result<()> {
    Ok(())
}

/// The language data the build bundled, unpacked, or none when it bundled none.
pub(crate) fn bundled_tessdata() -> Result<Option<PathBuf>, String> {
    if OCR_FILES.is_empty() {
        return Ok(None);
    }
    Ok(Some(unpacked()?.join(OCR_MODELS)))
}

/// The tesseract library the build bundled, unpacked, if it bundled one.
#[cfg(not(feature = "static"))]
pub(crate) fn bundled_tesseract_library() -> Result<Option<PathBuf>, String> {
    OCR_TESSERACT_LIBRARY
        .map(|library| Ok(unpacked()?.join(library)))
        .transpose()
}

/// The heif-png the build bundled, unpacked, if it bundled one.
#[cfg(not(feature = "static"))]
pub(crate) fn bundled_heif_decoder() -> Result<Option<PathBuf>, String> {
    OCR_HEIF_DECODER
        .map(|decoder| Ok(unpacked()?.join(decoder)))
        .transpose()
}

/// The version of the tesseract the build runs: the bundled library, or without a bundle the
/// program on the PATH.
#[cfg(not(feature = "static"))]
pub(crate) fn tesseract_version() -> Result<String, String> {
    match OCR_TESSERACT_LIBRARY {
        Some(library) => library_version(&unpacked()?.join(library)),
        None => program_version(Path::new("tesseract")),
    }
}

/// The version of the tesseract linked into the static executable, which native readings
/// and Tika's path both run.
#[cfg(feature = "static")]
pub(crate) fn tesseract_version() -> Result<String, String> {
    crate::ocr::tesseract().map(|tesseract| linked_version(&tesseract.version()))
}

#[cfg(not(feature = "static"))]
fn program_version(program: &Path) -> Result<String, String> {
    let output = process::Command::new(program)
        .arg("--version")
        .output()
        .map_err(|e| format!("Failed to run {}: {e}", program.display()))?;
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    ))
}

#[cfg(not(feature = "static"))]
fn library_version(library: &Path) -> Result<String, String> {
    let fail = |e: libloading::Error| format!("Failed to load {}: {e}", library.display());
    let loaded = unsafe { libloading::Library::new(library) }.map_err(fail)?;
    let version = unsafe {
        loaded.get::<unsafe extern "C" fn() -> *const std::ffi::c_char>(
            b"extractous_tesseract_version",
        )
    }
    .map_err(fail)?;
    Ok(linked_version(
        &unsafe { std::ffi::CStr::from_ptr(version()) }.to_string_lossy(),
    ))
}

fn linked_version(version: &str) -> String {
    format!("tesseract {version} (linked)")
}

/// The licenses of the tesseract and the language data the build bundled, or None when it
/// bundled none.
pub fn bundled_ocr_licenses() -> Option<&'static str> {
    OCR_LICENSES
}

/// The licenses of the native libraries linked into the static executable beyond tesseract's:
/// those of libheif with its decoders, and in the static image of Tika also AWT's fonts, or None
/// in a build that links none.
pub fn linked_native_licenses() -> Option<&'static str> {
    #[cfg(feature = "static")]
    let licenses = Some(LINKED_LICENSES);
    #[cfg(not(feature = "static"))]
    let licenses = None;
    licenses
}
