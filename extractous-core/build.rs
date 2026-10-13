use std::env;
use std::ffi::OsStr;
use std::fs;
use std::hash::{DefaultHasher, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

const TIKA_NATIVE_INPUTS: [&str; 5] = ["build.gradle", "gradle", "gradlew", "gradlew.bat", "src"];
const FINGERPRINT_FILE: &str = ".fingerprint";
const COMPRESSED_EXTENSION: &str = "gz";
const LIBS_LAYOUT: &[u8] = b"gzip-per-library, graalvm runpath entries as $ORIGIN";
const DT_RPATH: i64 = 15;
const DT_RUNPATH: i64 = 29;
const ELF64_DYN_SIZE: usize = 16;
const TIKA_LIBRARY_STEM: &str = "libtika_native";
const OCR_BUNDLE_VARIABLE: &str = "EXTRACTOUS_OCR_BUNDLE";
const OCR_TESSERACT: &str = "tesseract";
const OCR_MODELS: &str = "tessdata";
const OCR_MODEL_EXTENSION: &str = "traineddata";
const OCR_LICENSES: &str = "licenses";

fn main() {
    let source_dir = PathBuf::from(required_env("CARGO_MANIFEST_DIR")).join("tika-native");
    let out_dir = PathBuf::from(required_env("OUT_DIR"));
    for input in TIKA_NATIVE_INPUTS {
        println!(
            "cargo:rerun-if-changed={}",
            source_dir.join(input).display()
        );
    }
    println!("cargo:rerun-if-env-changed=GRAALVM_HOME");

    let host = required_env("HOST");
    let target = required_env("TARGET");
    if host != target {
        panic!(
            "Cross-compiling is not supported: GraalVM native-image builds tika_native for the \
             host {host}, not for the target {target}"
        );
    }

    let platform = Platform::from_target_os(&required_env("CARGO_CFG_TARGET_OS"));
    let graalvm_home = graalvm_home(platform);
    let fingerprint = fingerprint(&source_dir, &graalvm_home);
    let libs_dir = out_dir.join("libs");

    if read_fingerprint(&libs_dir).as_deref() != Some(fingerprint.as_str()) {
        match find_built_libs(&out_dir, &fingerprint) {
            Some(built_libs) => install_libs(&out_dir, &libs_dir, |staging_dir| {
                for path in list_dir(&built_libs) {
                    copy_file(&path, &staging_dir.join(path.file_name().unwrap()));
                }
            }),
            None => {
                let native_dir = gradle_build(&source_dir, &out_dir, &graalvm_home, platform);
                install_libs(&out_dir, &libs_dir, |staging_dir| {
                    compress_native_libraries(
                        &native_dir,
                        staging_dir,
                        &graalvm_home,
                        platform,
                        &fingerprint,
                    )
                });
            }
        }
    }

    let ocr = ocr_bundle(&out_dir);
    write_native_module(&out_dir, &libs_dir, &fingerprint, ocr.as_ref());
}

/// The OCR bundle `ci/ocr-bundle.sh` assembles, which `EXTRACTOUS_OCR_BUNDLE` names: a tesseract
/// executable, its language data and the licenses of what it links, compressed beside the
/// native libraries.
struct OcrBundle {
    /// Each file by its path below the unpacked libraries, and where its compressed copy is.
    files: Vec<(String, PathBuf)>,
    /// The licenses of the bundle, as one text.
    licenses: PathBuf,
    fingerprint: String,
}

fn ocr_bundle(out_dir: &Path) -> Option<OcrBundle> {
    println!("cargo:rerun-if-env-changed={OCR_BUNDLE_VARIABLE}");
    let dir = PathBuf::from(env::var_os(OCR_BUNDLE_VARIABLE)?);
    println!("cargo:rerun-if-changed={}", dir.display());
    let models: Vec<PathBuf> = list_dir(&dir.join(OCR_MODELS))
        .into_iter()
        .filter(|path| path.extension() == Some(OsStr::new(OCR_MODEL_EXTENSION)))
        .collect();
    if models.is_empty() {
        panic!(
            "{OCR_BUNDLE_VARIABLE} names {}, which holds no {OCR_MODELS}/*.{OCR_MODEL_EXTENSION}",
            dir.display()
        );
    }
    let compressed_dir = out_dir.join("ocr");
    remove_dir_if_exists(&compressed_dir);
    create_dir(&compressed_dir.join(OCR_MODELS));
    let mut hasher = DefaultHasher::new();
    let mut files = Vec::with_capacity(models.len() + 1);
    let named = std::iter::once((OCR_TESSERACT.to_string(), dir.join(OCR_TESSERACT))).chain(
        models.iter().map(|model| {
            let file = model
                .file_name()
                .unwrap()
                .to_str()
                .unwrap_or_else(|| panic!("OCR model name {} is not UTF-8", model.display()));
            (format!("{OCR_MODELS}/{file}"), model.clone())
        }),
    );
    for (name, path) in named {
        let content = read_file(&path);
        hasher.write(name.as_bytes());
        hasher.write(&content);
        let compressed = compressed_dir.join(format!("{name}.{COMPRESSED_EXTENSION}"));
        compress_bytes(&content, &compressed);
        files.push((name, compressed));
    }
    let mut licenses = String::new();
    for component in list_dir(&dir.join(OCR_LICENSES)) {
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
            "{OCR_BUNDLE_VARIABLE} names {}, which holds no licenses",
            dir.display()
        );
    }
    let licenses_path = compressed_dir.join("LICENSES.md");
    fs::write(&licenses_path, licenses)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", licenses_path.display()));
    Some(OcrBundle {
        files,
        licenses: licenses_path,
        fingerprint: format!("{:016x}", hasher.finish()),
    })
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

fn graalvm_home(platform: Platform) -> PathBuf {
    let home = env::var_os("GRAALVM_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!(
                "GRAALVM_HOME is not set. Building tika_native needs the GraalVM JDK that \
                 tika-native/build.gradle requires; the devenv shell provides it."
            )
        });
    let native_image = home.join("bin").join(if platform == Platform::Windows {
        "native-image.cmd"
    } else {
        "native-image"
    });
    if !native_image.is_file() {
        panic!(
            "GRAALVM_HOME={} is not a GraalVM JDK: {} does not exist",
            home.display(),
            native_image.display()
        );
    }
    home
}

fn read_file(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()))
}

fn fingerprint(source_dir: &Path, graalvm_home: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    hasher.write(LIBS_LAYOUT);
    let release = read_file(&graalvm_home.join("release"));
    hasher.write_usize(release.len());
    hasher.write(&release);
    for input in TIKA_NATIVE_INPUTS {
        for entry in WalkDir::new(source_dir.join(input)).sort_by_file_name() {
            let entry =
                entry.unwrap_or_else(|e| panic!("Failed to walk {}: {e}", source_dir.display()));
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry.path().strip_prefix(source_dir).unwrap();
            let name = relative.as_os_str().as_encoded_bytes();
            let content = read_file(entry.path());
            hasher.write_usize(name.len());
            hasher.write(name);
            hasher.write_usize(content.len());
            hasher.write(&content);
        }
    }
    format!("{:016x}", hasher.finish())
}

fn read_fingerprint(libs_dir: &Path) -> Option<String> {
    match fs::read_to_string(libs_dir.join(FINGERPRINT_FILE)) {
        Ok(fingerprint) => Some(fingerprint),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => panic!(
            "Failed to read the fingerprint in {}: {e}",
            libs_dir.display()
        ),
    }
}

fn find_built_libs(out_dir: &Path, fingerprint: &str) -> Option<PathBuf> {
    let build_dir = out_dir.parent()?.parent()?;
    list_dir(build_dir)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with("extractous-"))
        })
        .map(|path| path.join("out").join("libs"))
        .find(|libs| read_fingerprint(libs).as_deref() == Some(fingerprint))
}

fn gradle_build(
    source_dir: &Path,
    out_dir: &Path,
    graalvm_home: &Path,
    platform: Platform,
) -> PathBuf {
    let work_dir = out_dir.join("tika-native");
    remove_dir_if_exists(&work_dir);
    for input in TIKA_NATIVE_INPUTS {
        copy_tree(&source_dir.join(input), &work_dir.join(input));
    }

    let gradlew = work_dir.join(if platform == Platform::Windows {
        "gradlew.bat"
    } else {
        "gradlew"
    });
    println!(
        "Building tika_native with the GraalVM JDK at {}",
        graalvm_home.display()
    );
    let status = Command::new(&gradlew)
        .current_dir(&work_dir)
        .arg("--no-daemon")
        .arg("nativeCompile")
        .env("JAVA_HOME", graalvm_home)
        .status()
        .unwrap_or_else(|e| panic!("Failed to run {}: {e}", gradlew.display()));
    if !status.success() {
        panic!("{} nativeCompile failed: {status}", gradlew.display());
    }
    work_dir.join("build").join("native").join("nativeCompile")
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

fn compress_native_libraries(
    native_dir: &Path,
    staging_dir: &Path,
    graalvm_home: &Path,
    platform: Platform,
    fingerprint: &str,
) {
    for path in list_dir(native_dir) {
        if path.extension() != Some(OsStr::new(platform.dynamic_library_extension())) {
            continue;
        }
        let mut content = read_file(&path);
        relocate_graalvm_runpaths(&mut content, graalvm_home)
            .unwrap_or_else(|e| panic!("Failed to rewrite the runpath of {}: {e}", path.display()));
        let compressed = staging_dir
            .join(path.file_name().unwrap())
            .with_added_extension(COMPRESSED_EXTENSION);
        compress_bytes(&content, &compressed);
    }
    let fingerprint_path = staging_dir.join(FINGERPRINT_FILE);
    fs::write(&fingerprint_path, fingerprint)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", fingerprint_path.display()));
}

fn install_libs(out_dir: &Path, libs_dir: &Path, fill: impl FnOnce(&Path)) {
    let staging_dir = out_dir.join("libs.staging");
    remove_dir_if_exists(&staging_dir);
    create_dir(&staging_dir);
    fill(&staging_dir);

    remove_dir_if_exists(libs_dir);
    fs::rename(&staging_dir, libs_dir).unwrap_or_else(|e| {
        panic!(
            "Failed to move {} to {}: {e}",
            staging_dir.display(),
            libs_dir.display()
        )
    });
}

fn copy_tree(from: &Path, to: &Path) {
    for entry in WalkDir::new(from) {
        let entry = entry.unwrap_or_else(|e| panic!("Failed to walk {}: {e}", from.display()));
        let relative = entry.path().strip_prefix(from).unwrap();
        let dest = if relative.as_os_str().is_empty() {
            to.to_path_buf()
        } else {
            to.join(relative)
        };
        if entry.file_type().is_dir() {
            create_dir(&dest);
        } else {
            create_dir(dest.parent().unwrap());
            copy_file(entry.path(), &dest);
        }
    }
}

fn create_dir(dir: &Path) {
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("Failed to create {}: {e}", dir.display()));
}

fn copy_file(from: &Path, to: &Path) {
    fs::copy(from, to)
        .unwrap_or_else(|e| panic!("Failed to copy {} to {}: {e}", from.display(), to.display()));
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

fn relocate_graalvm_runpaths(content: &mut [u8], graalvm_home: &Path) -> Result<(), String> {
    use object::{Object, ObjectSection};

    let file = match object::File::parse(&*content) {
        Ok(file) if file.format() == object::BinaryFormat::Elf => file,
        _ => return Ok(()),
    };
    if !file.is_64() || !file.is_little_endian() {
        return Err("only 64-bit little-endian ELF is supported".to_string());
    }
    let section_range = |name: &str| {
        file.section_by_name(name)
            .and_then(|section| section.file_range())
            .map(|(offset, size)| (offset as usize, size as usize))
    };
    let (Some((dynstr_offset, _)), Some((dynamic_offset, dynamic_size))) =
        (section_range(".dynstr"), section_range(".dynamic"))
    else {
        return Ok(());
    };
    let (dynamic_entries, _) =
        content[dynamic_offset..dynamic_offset + dynamic_size].as_chunks::<ELF64_DYN_SIZE>();
    let runpath_offsets: Vec<usize> = dynamic_entries
        .iter()
        .map(|entry| {
            let tag = i64::from_le_bytes(entry[..8].try_into().unwrap());
            let value = u64::from_le_bytes(entry[8..].try_into().unwrap());
            (tag, value as usize)
        })
        .take_while(|&(tag, _)| tag != 0)
        .filter(|&(tag, _)| tag == DT_RUNPATH || tag == DT_RPATH)
        .map(|(_, value)| dynstr_offset + value)
        .collect();

    let graalvm_home = graalvm_home
        .to_str()
        .ok_or_else(|| format!("GRAALVM_HOME {} is not UTF-8", graalvm_home.display()))?;
    for start in runpath_offsets {
        let length = content[start..]
            .iter()
            .position(|&byte| byte == 0)
            .ok_or("unterminated runpath string")?;
        let old = std::str::from_utf8(&content[start..start + length])
            .map_err(|e| format!("runpath is not UTF-8: {e}"))?;
        let mut entries: Vec<&str> = Vec::new();
        for entry in old.split(':') {
            let entry = if entry.starts_with(graalvm_home) {
                "$ORIGIN"
            } else {
                entry
            };
            if !entries.contains(&entry) {
                entries.push(entry);
            }
        }
        let new = entries.join(":");
        content[start..start + new.len()].copy_from_slice(new.as_bytes());
        content[start + new.len()..start + length].fill(0);
    }
    Ok(())
}

fn write_native_module(
    out_dir: &Path,
    libs_dir: &Path,
    fingerprint: &str,
    ocr: Option<&OcrBundle>,
) {
    let mut libraries: Vec<(String, PathBuf)> = list_dir(libs_dir)
        .into_iter()
        .filter(|path| path.extension() == Some(OsStr::new(COMPRESSED_EXTENSION)))
        .map(|path| {
            let name =
                path.file_stem().unwrap().to_str().unwrap_or_else(|| {
                    panic!("Native library name {} is not UTF-8", path.display())
                });
            (name.to_owned(), path.clone())
        })
        .collect();
    libraries.sort();

    let tika_libraries: Vec<&str> = libraries
        .iter()
        .map(|(name, _)| name.as_str())
        .filter(|name| Path::new(name).file_stem() == Some(OsStr::new(TIKA_LIBRARY_STEM)))
        .collect();
    let [tika_library] = tika_libraries.as_slice() else {
        panic!(
            "Expected exactly one {TIKA_LIBRARY_STEM} library in {}, found {tika_libraries:?}",
            libs_dir.display()
        );
    };

    let entries: String = libraries
        .iter()
        .map(|(name, path)| {
            let path = path
                .to_str()
                .unwrap_or_else(|| panic!("Native library path {} is not UTF-8", path.display()));
            format!("    ({name:?}, include_bytes!({path:?})),\n")
        })
        .collect();
    let ocr_files: &[(String, PathBuf)] = ocr.map_or(&[], |ocr| &ocr.files);
    let ocr_entries: String = ocr_files
        .iter()
        .map(|(name, path)| format!("    ({name:?}, include_bytes!({path:?})),\n"))
        .collect();
    let ocr_licenses = match ocr {
        Some(ocr) => format!("Some(include_str!({:?}))", ocr.licenses),
        None => "None".to_string(),
    };
    let fingerprint = match ocr {
        Some(ocr) => format!("{fingerprint}-ocr-{}", ocr.fingerprint),
        None => fingerprint.to_string(),
    };
    let module = format!(
        "pub(crate) const NATIVE_FINGERPRINT: &str = {fingerprint:?};\n\
         pub(crate) const TIKA_LIBRARY: &str = {tika_library:?};\n\
         pub(crate) static NATIVE_LIBRARIES: [(&str, &[u8]); {}] = [\n{entries}];\n\
         pub(crate) const OCR_TESSERACT: &str = {OCR_TESSERACT:?};\n\
         pub(crate) const OCR_MODELS: &str = {OCR_MODELS:?};\n\
         pub(crate) static OCR_FILES: [(&str, &[u8]); {}] = [\n{ocr_entries}];\n\
         pub(crate) const OCR_LICENSES: Option<&str> = {ocr_licenses};\n",
        libraries.len(),
        ocr_files.len()
    );
    let module_path = out_dir.join("native.rs");
    if fs::read_to_string(&module_path).ok().as_deref() != Some(module.as_str()) {
        fs::write(&module_path, module)
            .unwrap_or_else(|e| panic!("Failed to write {}: {e}", module_path.display()));
    }
}
