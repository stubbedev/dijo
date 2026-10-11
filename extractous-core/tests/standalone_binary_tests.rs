use extractous::Extractor;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const CHILD_MARKER: &str = "EXTRACTOUS_STANDALONE_CHILD";
const DOCUMENT: &str = "simple.odt";

fn document_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../test_files/documents")
        .join(DOCUMENT)
}

#[test]
fn extracts_in_a_copied_binary_without_the_cargo_environment() {
    if env::var_os(CHILD_MARKER).is_some() {
        let (content, _) = Extractor::new()
            .extract_file_to_string(document_path().to_str().unwrap())
            .unwrap();
        print!("{content}");
        return;
    }

    let scratch = env::temp_dir().join(format!("extractous-standalone-{}", std::process::id()));
    let cache = scratch.join("cache");
    fs::create_dir_all(&cache).unwrap();
    let binary = scratch.join("standalone");
    fs::copy(env::current_exe().unwrap(), &binary).unwrap();

    let output = Command::new(&binary)
        .args([
            "--exact",
            "extracts_in_a_copied_binary_without_the_cargo_environment",
            "--nocapture",
            "--test-threads=1",
        ])
        .env_clear()
        .env(CHILD_MARKER, "1")
        .env("XDG_CACHE_HOME", &cache)
        .current_dir(&scratch)
        .output()
        .unwrap();
    fs::remove_dir_all(&scratch).unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (expected, _) = Extractor::new()
        .extract_file_to_string(document_path().to_str().unwrap())
        .unwrap();
    assert!(stdout.contains(expected.trim()), "{stdout}");
}
