//! The files the build embedded in the program, the OCR bundle (`DIJO_OCR_BUNDLE`), unpacked
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
pub fn unpacked() -> Result<&'static Path, String> {
    static UNPACKED: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    UNPACKED
        .get_or_init(install_dir)
        .as_ref()
        .map(PathBuf::as_path)
        .map_err(Clone::clone)
}

/// Every embedded file with its path below the unpacked directory.
fn embedded_files() -> impl Iterator<Item = &'static (&'static str, &'static [u8])> {
    OCR_FILES.iter()
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
    let root = cache_dir()?.join("dijo");
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

/// The embedded files that are programs the build runs: the bundled heif-png, where the build
/// bundles one.
#[cfg(all(not(feature = "static"), not(target_os = "macos")))]
const PROGRAMS: &[&str] = &[OCR_HEIF_DECODER];
#[cfg(any(feature = "static", target_os = "macos"))]
const PROGRAMS: &[&str] = &[];

/// Whether an embedded file is a program the build runs.
fn is_program(name: &str) -> bool {
    PROGRAMS.contains(&name)
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

/// The language data the build bundled, unpacked.
pub fn bundled_tessdata() -> Result<PathBuf, String> {
    Ok(unpacked()?.join(OCR_MODELS))
}

/// The tesseract library the build bundled, unpacked.
#[cfg(not(feature = "static"))]
pub fn bundled_tesseract_library() -> Result<PathBuf, String> {
    Ok(unpacked()?.join(OCR_TESSERACT_LIBRARY))
}

/// The heif-png the build bundled, unpacked.
#[cfg(all(not(feature = "static"), not(target_os = "macos")))]
pub fn bundled_heif_decoder() -> Result<PathBuf, String> {
    Ok(unpacked()?.join(OCR_HEIF_DECODER))
}

/// What the system's loader said of a library it could not load, or of a symbol it could not
/// find: libloading's error followed by the loader's own message beneath it.
#[cfg(not(feature = "static"))]
pub fn loader_message(error: &libloading::Error) -> String {
    let mut message = error.to_string();
    let mut cause = std::error::Error::source(error);
    while let Some(error) = cause {
        message.push_str(": ");
        message.push_str(&error.to_string());
        cause = error.source();
    }
    message
}

pub fn linked_version(version: &str) -> String {
    format!("tesseract {version} (linked)")
}

/// The licenses of the tesseract and the language data the build bundled.
pub fn bundled_ocr_licenses() -> &'static str {
    OCR_LICENSES
}

/// The licenses of the native libraries linked into the static executable beyond tesseract's:
/// those of libheif with its decoders, or None
/// in a build that links none.
pub fn linked_native_licenses() -> Option<&'static str> {
    #[cfg(feature = "static")]
    let licenses = Some(LINKED_LICENSES);
    #[cfg(not(feature = "static"))]
    let licenses = None;
    licenses
}
