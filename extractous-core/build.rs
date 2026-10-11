use std::env;
use std::ffi::OsStr;
use std::fs;
use std::hash::{DefaultHasher, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

const TIKA_NATIVE_INPUTS: [&str; 5] = ["build.gradle", "gradle", "gradlew", "gradlew.bat", "src"];
const FINGERPRINT_FILE: &str = ".fingerprint";

fn main() {
    if env::var_os("DOCS_RS").is_some() {
        return;
    }

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

    let windows = required_env("CARGO_CFG_TARGET_OS") == "windows";
    let graalvm_home = graalvm_home(windows);
    let fingerprint = fingerprint(&source_dir, &graalvm_home);
    let libs_dir = out_dir.join("libs");

    if read_fingerprint(&libs_dir).as_deref() != Some(fingerprint.as_str()) {
        let built = find_built_libs(&out_dir, &fingerprint)
            .unwrap_or_else(|| gradle_build(&source_dir, &out_dir, &graalvm_home, windows));
        install_libs(&built, &out_dir, &libs_dir, &fingerprint);
    }

    println!("cargo:rustc-link-search={}", libs_dir.display());
    let lib_tika_name = if windows {
        "libtika_native"
    } else {
        "tika_native"
    };
    println!("cargo:rustc-link-lib=dylib={lib_tika_name}");
}

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|e| panic!("Cargo did not provide {name}: {e}"))
}

fn graalvm_home(windows: bool) -> PathBuf {
    let home = env::var_os("GRAALVM_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!(
                "GRAALVM_HOME is not set. Building tika_native needs the GraalVM JDK that \
                 tika-native/build.gradle requires; the devenv shell provides it."
            )
        });
    let native_image = home.join("bin").join(if windows {
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
    fs::read_dir(build_dir)
        .unwrap_or_else(|e| panic!("Failed to list {}: {e}", build_dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| panic!("Failed to list {}: {e}", build_dir.display()))
                .path()
        })
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with("extractous-"))
        })
        .map(|path| path.join("out").join("libs"))
        .find(|libs| read_fingerprint(libs).as_deref() == Some(fingerprint))
}

fn gradle_build(source_dir: &Path, out_dir: &Path, graalvm_home: &Path, windows: bool) -> PathBuf {
    let work_dir = out_dir.join("tika-native");
    remove_dir_if_exists(&work_dir);
    for input in TIKA_NATIVE_INPUTS {
        copy_tree(&source_dir.join(input), &work_dir.join(input));
    }

    let gradlew = work_dir.join(if windows { "gradlew.bat" } else { "gradlew" });
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

fn install_libs(from_dir: &Path, out_dir: &Path, libs_dir: &Path, fingerprint: &str) {
    let staging_dir = out_dir.join("libs.staging");
    remove_dir_if_exists(&staging_dir);
    create_dir(&staging_dir);

    for entry in fs::read_dir(from_dir)
        .unwrap_or_else(|e| panic!("Failed to list {}: {e}", from_dir.display()))
    {
        let path = entry
            .unwrap_or_else(|e| panic!("Failed to list {}: {e}", from_dir.display()))
            .path();
        let name = path.file_name().unwrap();
        if path.extension() == Some(OsStr::new("h")) || name == FINGERPRINT_FILE {
            continue;
        }
        copy_file(&path, &staging_dir.join(name));
    }
    let fingerprint_path = staging_dir.join(FINGERPRINT_FILE);
    fs::write(&fingerprint_path, fingerprint)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", fingerprint_path.display()));

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
