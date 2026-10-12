#![allow(dead_code)]

use std::fs;
use std::path::PathBuf;

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
        use std::process::Command;
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

/// A ZIP archive of `entries`, stored without compression, which is all a test needs to build an
/// Office document.
pub fn stored_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    const VERSION: u16 = 20;
    let mut archive = Vec::new();
    let mut directory = Vec::new();
    for (name, data) in entries {
        let offset = archive.len() as u32;
        let size = data.len() as u32;
        let mut common = Vec::new();
        for field in [VERSION, 0, 0, 0, 0] {
            common.extend_from_slice(&field.to_le_bytes());
        }
        for field in [crc32(data), size, size] {
            common.extend_from_slice(&field.to_le_bytes());
        }
        for field in [name.len() as u16, 0] {
            common.extend_from_slice(&field.to_le_bytes());
        }

        archive.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        archive.extend_from_slice(&common);
        archive.extend_from_slice(name.as_bytes());
        archive.extend_from_slice(data);

        directory.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        directory.extend_from_slice(&VERSION.to_le_bytes());
        directory.extend_from_slice(&common);
        for field in [0u16, 0, 0] {
            directory.extend_from_slice(&field.to_le_bytes());
        }
        for field in [0u32, offset] {
            directory.extend_from_slice(&field.to_le_bytes());
        }
        directory.extend_from_slice(name.as_bytes());
    }
    let directory_offset = archive.len() as u32;
    archive.extend_from_slice(&directory);
    archive.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    for field in [0u16, 0, entries.len() as u16, entries.len() as u16] {
        archive.extend_from_slice(&field.to_le_bytes());
    }
    for field in [directory.len() as u32, directory_offset] {
        archive.extend_from_slice(&field.to_le_bytes());
    }
    archive.extend_from_slice(&0u16.to_le_bytes());
    archive
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}
