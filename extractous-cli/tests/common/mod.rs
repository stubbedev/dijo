#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

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

/// A PDF of `pages` pages of plain text, large enough that parsing all of it takes a while.
pub fn text_pdf(pages: usize) -> Vec<u8> {
    let content = {
        let mut lines = String::from("BT /F1 10 Tf 12 TL 40 800 Td");
        for line in 0..60 {
            lines.push_str(&format!(
                " (Line {line} of a long generated document, written to keep the parser busy.) Tj T*"
            ));
        }
        lines.push_str(" ET");
        lines
    };
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            (0..pages)
                .map(|page| format!("{} 0 R", 5 + page))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
    ];
    objects.extend((0..pages).map(|_| {
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] \
         /Resources << /Font << /F1 3 0 R >> >> /Contents 4 0 R >>"
            .to_string()
    }));
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in offsets {
        pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

/// Characters of scripts written without spaces between words count as tokens of their own.
fn unspaced(character: char) -> bool {
    matches!(
        character,
        '\u{0e00}'..='\u{0e7f}'
            | '\u{3040}'..='\u{30ff}'
            | '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
    )
}

pub fn tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    for character in text.nfc() {
        if unspaced(character) {
            tokens.extend((!word.is_empty()).then(|| std::mem::take(&mut word)));
            tokens.push(character.to_string());
        } else if character.is_alphanumeric() || is_combining_mark(character) {
            word.push(character);
        } else if !word.is_empty() {
            tokens.push(std::mem::take(&mut word));
        }
    }
    tokens.extend((!word.is_empty()).then_some(word));
    tokens
}

/// How many of `truth` occur in `extracted`, each occurrence matched once.
pub fn matched(truth: &[String], extracted: &[String]) -> usize {
    let mut available: HashMap<&str, usize> = HashMap::new();
    for token in extracted {
        *available.entry(token).or_default() += 1;
    }
    truth
        .iter()
        .filter(|token| {
            available
                .get_mut(token.as_str())
                .filter(|count| **count > 0)
                .map(|count| *count -= 1)
                .is_some()
        })
        .count()
}
