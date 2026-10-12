#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub const DOCUMENTS: &str = "../test_files/documents";

pub fn document(name: &str) -> String {
    format!("{DOCUMENTS}/{name}")
}

/// A directory of its own for one test, removed when dropped.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(name: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("extractous-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// A PATH on which `tesseract` first runs `before`, a shell snippet that sees tesseract's
    /// arguments as `$@`, and then the real tesseract.
    #[cfg(unix)]
    pub fn path_with_tesseract(&self, before: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let real = String::from_utf8(
            Command::new("sh")
                .args(["-c", "command -v tesseract"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let real = real.trim();
        assert!(!real.is_empty(), "tesseract is not installed");
        let fake = self.0.join("tesseract");
        fs::write(
            &fake,
            format!("#!/bin/sh\n{before}\nexec '{real}' \"$@\"\n"),
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        format!(
            "{}:{}",
            self.0.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
