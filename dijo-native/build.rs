use std::env;
use std::ffi::OsStr;
use std::fs;
use std::hash::{DefaultHasher, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const COMPRESSED_EXTENSION: &str = "gz";
const OCR_BUNDLE_VARIABLE: &str = "DIJO_OCR_BUNDLE";
const OCR_TESSERACT_LIBRARY_STEM: &str = "dijo_tesseract";
const OCR_HEIF_DECODER_STEM: &str = "heif-png";
const OCR_MODELS: &str = "tessdata";
const OCR_MODEL_EXTENSION: &str = "traineddata";
const OCR_RECOGNIZERS: &str = "ppocr";
const OCR_LICENSES: &str = "licenses";
const STATIC_LICENSES_VARIABLE: &str = "DIJO_STATIC_LICENSES";
const STATIC_LIBS_VARIABLE: &str = "DIJO_STATIC_LIBS";
/// The C++ runtime tesseract and libheif need, which `pkg-config` leaves out, and GCC's runtime,
/// whose CPU detection (`__builtin_cpu_supports`) tesseract picks its kernels with.
const STATIC_RUNTIME_LIBRARIES: [&str; 2] = ["stdc++", "gcc"];
/// The libraries of the C library itself, which musl's `libc.a` holds.
const STATIC_C_LIBRARIES: [&str; 4] = ["c", "m", "pthread", "dl"];
fn main() {
    let out_dir = PathBuf::from(required_env("OUT_DIR"));
    let platform = Platform::from_target_os(&required_env("CARGO_CFG_TARGET_OS"));
    if env::var_os("CARGO_FEATURE_STATIC").is_some() {
        let ocr = ocr_bundle(&out_dir, platform, Tesseract::Linked);
        let linked_licenses = static_licenses(&out_dir);
        link_static_libraries();
        write_static_native_module(&out_dir, &ocr, &linked_licenses);
    } else {
        let ocr = ocr_bundle(&out_dir, platform, platform.bundled_tesseract());
        write_ocr_module(&out_dir, &ocr);
    }
}

/// Has Cargo link the static libraries `DIJO_STATIC_LIBS` lists into the program: the files
/// it names, separated as `PATH` is, hold one linker flag per line, `-L<dir>`, `-l<library>` or
/// `-pthread`, as `pkg-config --static --libs` prints them. A library named more than once is
/// linked where it is last named, after everything that needs it, and those of the C library
/// itself (`STATIC_C_LIBRARIES`), which musl's holds whole, are left to it.
fn link_static_libraries() {
    println!("cargo:rerun-if-env-changed={STATIC_LIBS_VARIABLE}");
    let files = env::var_os(STATIC_LIBS_VARIABLE).unwrap_or_else(|| {
        panic!("The static build needs {STATIC_LIBS_VARIABLE} for the libraries it links")
    });
    let mut libraries: Vec<String> = Vec::new();
    for file in env::split_paths(&files) {
        println!("cargo:rerun-if-changed={}", file.display());
        let flags = String::from_utf8(read_file(&file))
            .unwrap_or_else(|e| panic!("{} is not UTF-8: {e}", file.display()));
        for flag in flags.lines() {
            if let Some(dir) = flag.strip_prefix("-L") {
                println!("cargo:rustc-link-search=native={dir}");
            } else if let Some(library) = flag.strip_prefix("-l") {
                libraries.push(library.to_owned());
            } else if flag != "-pthread" {
                panic!(
                    "{} holds {flag:?}, which is no library to link",
                    file.display()
                );
            }
        }
    }
    libraries.extend(STATIC_RUNTIME_LIBRARIES.map(str::to_owned));
    for (index, library) in libraries.iter().enumerate() {
        if STATIC_C_LIBRARIES.contains(&library.as_str())
            || libraries[index + 1..].contains(library)
        {
            continue;
        }
        println!("cargo:rustc-link-lib=static:-bundle={library}");
    }
}

/// The OCR bundle `ci/ocr-bundle.sh` assembles, which `DIJO_OCR_BUNDLE` names: tesseract as
/// a library of the C API native readings call unless the static executable links it, heif-png
/// on the platforms that need it, the language data, the PP-OCR models pages tesseract reads
/// poorly are read again with, and the licenses of what they hold, each compressed.
struct OcrBundle {
    /// How the built program runs the bundled tesseract.
    tesseract: Tesseract,
    /// Each file by its path below the unpacked libraries, and where its compressed copy is.
    files: Vec<(String, PathBuf)>,
    /// The embedded heif-png's file name, if one is embedded.
    heif_decoder: Option<String>,
    /// The licenses of the bundle, as one text.
    licenses: PathBuf,
    fingerprint: String,
}

/// How the built program runs tesseract, which decides what it embeds of the OCR bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tesseract {
    /// The bundled library of tesseract's C API of this name, unpacked and loaded into the process.
    Library(String),
    /// tesseract linked into the static executable, which needs only the language data.
    Linked,
}

impl Tesseract {
    /// The bundled library, unless tesseract is linked into the static executable.
    fn library(&self) -> Option<&str> {
        match self {
            Tesseract::Library(file) => Some(file),
            Tesseract::Linked => None,
        }
    }
}

/// Every build reads with the tesseract it carries, never one it finds where it runs, so a build
/// without the bundle fails.
fn ocr_bundle(out_dir: &Path, platform: Platform, tesseract: Tesseract) -> OcrBundle {
    println!("cargo:rerun-if-env-changed={OCR_BUNDLE_VARIABLE}");
    let dir = PathBuf::from(env::var_os(OCR_BUNDLE_VARIABLE).unwrap_or_else(|| {
        panic!(
            "dijo embeds the tesseract it reads with: {OCR_BUNDLE_VARIABLE} must name an OCR \
             bundle, as the development shell sets it or `just ocr-bundle` assembles one"
        )
    }));
    println!("cargo:rerun-if-changed={}", dir.display());
    let models = bundled_models(&dir, OCR_MODELS, Some(OCR_MODEL_EXTENSION));
    let recognizers = bundled_models(&dir, OCR_RECOGNIZERS, None);
    let compressed_dir = out_dir.join("ocr");
    remove_dir_if_exists(&compressed_dir);
    create_dir(&compressed_dir.join(OCR_MODELS));
    let mut hasher = DefaultHasher::new();
    let mut files = Vec::with_capacity(models.len() + recognizers.len() + 2);
    let heif_decoder = (tesseract != Tesseract::Linked && platform.heif_decoder_bundled())
        .then(|| platform.executable(OCR_HEIF_DECODER_STEM));
    let named = tesseract
        .library()
        .map(str::to_string)
        .into_iter()
        .chain(heif_decoder.clone())
        .map(|name| {
            let path = dir.join(&name);
            (name, path)
        })
        .chain(models)
        .chain(recognizers);
    for (name, path) in named {
        let content = read_file(&path);
        hasher.write(name.as_bytes());
        hasher.write(&content);
        let compressed = compressed_dir.join(format!("{name}.{COMPRESSED_EXTENSION}"));
        if let Some(parent) = compressed.parent() {
            create_dir(parent);
        }
        compress_bytes(&content, &compressed);
        files.push((name, compressed));
    }
    let licenses = compressed_dir.join("LICENSES.md");
    write_licenses(&dir.join(OCR_LICENSES), OCR_BUNDLE_VARIABLE, &licenses);
    OcrBundle {
        tesseract,
        files,
        heif_decoder,
        licenses,
        fingerprint: format!("{:016x}", hasher.finish()),
    }
}

/// The licenses of the native libraries the static build links beyond tesseract's, which
/// `DIJO_STATIC_LICENSES` names as a directory with one subdirectory per library, written
/// into one file.
fn static_licenses(out_dir: &Path) -> PathBuf {
    println!("cargo:rerun-if-env-changed={STATIC_LICENSES_VARIABLE}");
    let dir = PathBuf::from(env::var_os(STATIC_LICENSES_VARIABLE).unwrap_or_else(|| {
        panic!(
            "The static build needs {STATIC_LICENSES_VARIABLE} for the licenses of what it links"
        )
    }));
    println!("cargo:rerun-if-changed={}", dir.display());
    let licenses = out_dir.join("LINKED-LICENSES.md");
    write_licenses(&dir, STATIC_LICENSES_VARIABLE, &licenses);
    licenses
}

/// Writes every license file in `dir`, which holds one subdirectory per component, as one
/// Markdown text; `variable` names where `dir` came from.
fn write_licenses(dir: &Path, variable: &str, target: &Path) {
    let mut licenses = String::new();
    for component in list_dir(dir) {
        for file in list_dir(&component) {
            let text = String::from_utf8_lossy(&read_file(&file)).into_owned();
            licenses.push_str(&format!(
                "## {} ({})\n\n```text\n{}\n```\n\n",
                component.file_name().unwrap().to_string_lossy(),
                file.file_name().unwrap().to_string_lossy(),
                text.trim_end()
            ));
        }
    }
    if licenses.is_empty() {
        panic!(
            "{variable} names {}, which holds no licenses",
            dir.display()
        );
    }
    fs::write(target, licenses)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", target.display()));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    Windows,
    MacOs,
    OtherUnix,
}

impl Platform {
    fn from_target_os(target_os: &str) -> Self {
        match target_os {
            "windows" => Platform::Windows,
            "macos" => Platform::MacOs,
            _ => Platform::OtherUnix,
        }
    }

    /// The bundled tesseract of a build that is not the static executable: the library
    /// ci/ocr-vcpkg.sh builds, which the process loads.
    fn bundled_tesseract(self) -> Tesseract {
        Tesseract::Library(self.dynamic_library(OCR_TESSERACT_LIBRARY_STEM))
    }

    /// Whether the OCR bundle carries heif-png (dijo-native/heif-png): everywhere but
    /// macOS, whose own sips decodes HEIC, HEIF and AVIF.
    fn heif_decoder_bundled(self) -> bool {
        self != Platform::MacOs
    }

    /// The file name of an executable.
    fn executable(self, stem: &str) -> String {
        match self {
            Platform::Windows => format!("{stem}.exe"),
            Platform::MacOs | Platform::OtherUnix => stem.to_string(),
        }
    }

    /// The file name of a dynamic library.
    fn dynamic_library(self, stem: &str) -> String {
        match self {
            Platform::Windows => format!("{stem}.dll"),
            Platform::MacOs | Platform::OtherUnix => {
                format!("lib{stem}.{}", self.dynamic_library_extension())
            }
        }
    }

    fn dynamic_library_extension(self) -> &'static str {
        match self {
            Platform::Windows => "dll",
            Platform::MacOs => "dylib",
            Platform::OtherUnix => "so",
        }
    }
}

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|e| panic!("Cargo did not provide {name}: {e}"))
}

fn read_file(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()))
}

fn bundled_models(bundle: &Path, kind: &str, extension: Option<&str>) -> Vec<(String, PathBuf)> {
    let models = models_under(&bundle.join(kind), kind, extension);
    if models.is_empty() {
        panic!(
            "{OCR_BUNDLE_VARIABLE} names {}, which holds no {kind}/**/*.{}",
            bundle.display(),
            extension.unwrap_or("*")
        );
    }
    models
}

/// The files under `dir` with `extension`, or all of them, in subdirectories too (tesseract's
/// script models are in `script/`), each named by its path below `prefix` with `/` separators, in
/// sorted order.
fn models_under(dir: &Path, prefix: &str, extension: Option<&str>) -> Vec<(String, PathBuf)> {
    let mut models = Vec::new();
    for path in list_dir(dir) {
        let file = path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_else(|| panic!("OCR model path {} is not UTF-8", path.display()));
        let name = format!("{prefix}/{file}");
        if path.is_dir() {
            models.extend(models_under(&path, &name, extension));
        } else if extension.is_none_or(|extension| path.extension() == Some(OsStr::new(extension)))
        {
            models.push((name, path));
        }
    }
    models
}

/// The entries of a directory, sorted, so that what is built from them does not depend on the
/// order the file system lists them in.
fn list_dir(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("Failed to list {}: {e}", dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| panic!("Failed to list {}: {e}", dir.display()))
                .path()
        })
        .collect();
    entries.sort();
    entries
}

fn create_dir(dir: &Path) {
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("Failed to create {}: {e}", dir.display()));
}

fn remove_dir_if_exists(dir: &Path) {
    match fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => panic!("Failed to remove {}: {e}", dir.display()),
    }
}

fn compress_bytes(content: &[u8], to: &Path) {
    let file =
        fs::File::create(to).unwrap_or_else(|e| panic!("Failed to create {}: {e}", to.display()));
    let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    encoder
        .write_all(content)
        .and_then(|()| encoder.finish())
        .unwrap_or_else(|e| panic!("Failed to compress into {}: {e}", to.display()));
}

/// The module of the static build: only the OCR bundle's language data, since tesseract is
/// linked in, and the licenses of the native libraries linked in.
fn write_static_native_module(out_dir: &Path, ocr: &OcrBundle, linked_licenses: &Path) {
    let module = format!(
        "pub const NATIVE_FINGERPRINT: &str = {:?};\n\
         pub const LINKED_LICENSES: &str = include_str!({linked_licenses:?});\n{}",
        format!("static-ocr-{}", ocr.fingerprint),
        ocr_items(ocr)
    );
    write_if_changed(&out_dir.join("native.rs"), &module);
}

/// The module of a build that links nothing statically: only the OCR bundle.
fn write_ocr_module(out_dir: &Path, ocr: &OcrBundle) {
    let module = format!(
        "pub const NATIVE_FINGERPRINT: &str = {:?};\n{}",
        format!("ocr-{}", ocr.fingerprint),
        ocr_items(ocr)
    );
    write_if_changed(&out_dir.join("native.rs"), &module);
}

/// The items of the generated module that describe the OCR bundle: the tesseract library it
/// carries, unless tesseract is linked in, its heif-png, its language data, its PP-OCR models and
/// its licenses.
fn ocr_items(ocr: &OcrBundle) -> String {
    let ocr_entries: String = ocr
        .files
        .iter()
        .map(|(name, path)| format!("    ({name:?}, include_bytes!({path:?})),\n"))
        .collect();
    let library = ocr
        .tesseract
        .library()
        .map(|library| format!("pub const OCR_TESSERACT_LIBRARY: &str = {library:?};\n"))
        .unwrap_or_default();
    let heif_decoder = ocr
        .heif_decoder
        .as_ref()
        .map(|decoder| format!("pub const OCR_HEIF_DECODER: &str = {decoder:?};\n"))
        .unwrap_or_default();
    format!(
        "{library}{heif_decoder}\
         pub const OCR_MODELS: &str = {OCR_MODELS:?};\n\
         pub const OCR_RECOGNIZERS: &str = {OCR_RECOGNIZERS:?};\n\
         pub static OCR_FILES: [(&str, &[u8]); {}] = [\n{ocr_entries}];\n\
         pub const OCR_LICENSES: &str = include_str!({:?});\n",
        ocr.files.len(),
        ocr.licenses
    )
}

fn write_if_changed(path: &Path, content: &str) {
    if fs::read_to_string(path).ok().as_deref() != Some(content) {
        fs::write(path, content)
            .unwrap_or_else(|e| panic!("Failed to write {}: {e}", path.display()));
    }
}
