#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use unicode_normalization::char::is_combining_mark;
use unicode_normalization::UnicodeNormalization;

pub mod server;

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
        self.tesseract_wrapper(before);
        format!(
            "{}:{}",
            self.0.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    /// A PATH holding tesseract and nothing else.
    #[cfg(unix)]
    pub fn path_with_only_tesseract(&self) -> String {
        self.tesseract_wrapper("");
        self.0.display().to_string()
    }

    /// Writes a tesseract into this directory that runs `before`, then the installed one.
    #[cfg(unix)]
    fn tesseract_wrapper(&self, before: &str) {
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
        self.program("tesseract", &format!("{before}\nexec '{real}' \"$@\""));
    }

    /// Writes an executable shell script named `name` into this directory.
    #[cfg(unix)]
    pub fn program(&self, name: &str, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = self.0.join(name);
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Writes a ZIP archive of entries stored without compression, which is all a test needs to build
/// an Office document. Each entry's data is streamed and its size and checksum filled in after,
/// so an archive can be larger than memory, up to the 4 GiB of a ZIP without ZIP64.
pub struct StoredZip<W: Write + Seek> {
    out: W,
    directory: Vec<u8>,
    entries: u16,
}

const ZIP_VERSION: u16 = 20;

fn zip_u32(value: u64) -> u32 {
    u32::try_from(value).expect("the archive outgrows a ZIP without ZIP64")
}

impl<W: Write + Seek> StoredZip<W> {
    pub fn new(out: W) -> Self {
        Self {
            out,
            directory: Vec::new(),
            entries: 0,
        }
    }

    /// Adds the entry `name`, whose data `write` writes.
    pub fn entry(
        &mut self,
        name: &str,
        write: impl FnOnce(&mut dyn Write) -> io::Result<()>,
    ) -> io::Result<()> {
        let offset = self.out.stream_position()?;
        let header = |crc: u32, size: u32| {
            let mut fields = Vec::new();
            for field in [ZIP_VERSION, 0, 0, 0, 0] {
                fields.extend_from_slice(&field.to_le_bytes());
            }
            for field in [crc, size, size] {
                fields.extend_from_slice(&field.to_le_bytes());
            }
            for field in [name.len() as u16, 0] {
                fields.extend_from_slice(&field.to_le_bytes());
            }
            fields
        };
        self.out.write_all(&0x0403_4b50u32.to_le_bytes())?;
        self.out.write_all(&header(0, 0))?;
        self.out.write_all(name.as_bytes())?;
        let mut data = Checksummed {
            out: &mut self.out,
            hasher: crc32fast::Hasher::new(),
            size: 0,
        };
        write(&mut data)?;
        let (crc, size) = (data.hasher.finalize(), zip_u32(data.size));
        let end = self.out.stream_position()?;
        self.out.seek(SeekFrom::Start(offset + 4))?;
        self.out.write_all(&header(crc, size))?;
        self.out.seek(SeekFrom::Start(end))?;

        self.directory
            .extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        self.directory.extend_from_slice(&ZIP_VERSION.to_le_bytes());
        self.directory.extend_from_slice(&header(crc, size));
        for field in [0u16, 0, 0] {
            self.directory.extend_from_slice(&field.to_le_bytes());
        }
        for field in [0u32, zip_u32(offset)] {
            self.directory.extend_from_slice(&field.to_le_bytes());
        }
        self.directory.extend_from_slice(name.as_bytes());
        self.entries += 1;
        Ok(())
    }

    /// Writes the central directory and returns the output.
    pub fn finish(mut self) -> io::Result<W> {
        let directory_offset = zip_u32(self.out.stream_position()?);
        self.out.write_all(&self.directory)?;
        self.out.write_all(&0x0605_4b50u32.to_le_bytes())?;
        for field in [0u16, 0, self.entries, self.entries] {
            self.out.write_all(&field.to_le_bytes())?;
        }
        for field in [zip_u32(self.directory.len() as u64), directory_offset] {
            self.out.write_all(&field.to_le_bytes())?;
        }
        self.out.write_all(&0u16.to_le_bytes())?;
        Ok(self.out)
    }
}

/// Passes data on while summing its CRC-32 and size.
struct Checksummed<'w, W> {
    out: &'w mut W,
    hasher: crc32fast::Hasher,
    size: u64,
}

impl<W: Write> Write for Checksummed<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.out.write(buffer)?;
        self.hasher.update(&buffer[..written]);
        self.size += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

const OOXML: &str = "http://schemas.openxmlformats.org";

/// Writes a DOCX into `out`: `doctype` (empty for none) declares the document part's DTD, and
/// `body` writes what goes inside its `w:body`.
pub fn write_docx<W: Write + Seek>(
    out: W,
    doctype: &str,
    body: impl FnOnce(&mut dyn Write) -> io::Result<()>,
) -> io::Result<W> {
    let mut zip = StoredZip::new(out);
    zip.entry("[Content_Types].xml", |out| {
        write!(
            out,
            "<?xml version=\"1.0\"?><Types xmlns=\"{OOXML}/package/2006/content-types\">\
             <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
             <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/></Types>"
        )
    })?;
    zip.entry("_rels/.rels", |out| {
        write!(
            out,
            "<?xml version=\"1.0\"?><Relationships xmlns=\"{OOXML}/package/2006/relationships\">\
             <Relationship Id=\"rId1\" Type=\"{OOXML}/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/></Relationships>"
        )
    })?;
    zip.entry("word/document.xml", |out| {
        write!(
            out,
            "<?xml version=\"1.0\"?>{doctype}\
             <w:document xmlns:w=\"{OOXML}/wordprocessingml/2006/main\"><w:body>"
        )?;
        body(out)?;
        write!(out, "</w:body></w:document>")
    })?;
    zip.finish()
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

/// A small, seedable generator, so that a failing run can be repeated with its seed.
pub struct XorShift(pub u64);

impl XorShift {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    pub fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

/// The document damaged in one of the ways files get damaged: bytes flipped, the end cut off,
/// a stretch overwritten with noise, or a stretch repeated.
pub fn mutate(original: &[u8], random: &mut XorShift) -> Vec<u8> {
    let mut bytes = original.to_vec();
    if bytes.is_empty() {
        return bytes;
    }
    match random.below(4) {
        0 => {
            for _ in 0..=random.below(16) {
                let index = random.below(bytes.len());
                bytes[index] ^= 1 << random.below(8);
            }
        }
        1 => bytes.truncate(random.below(bytes.len())),
        2 => {
            let start = random.below(bytes.len());
            let end = (start + 1 + random.below(512)).min(bytes.len());
            for byte in &mut bytes[start..end] {
                *byte = random.next() as u8;
            }
        }
        _ => {
            let start = random.below(bytes.len());
            let end = (start + 1 + random.below(4096)).min(bytes.len());
            let chunk = bytes[start..end].to_vec();
            let at = random.below(bytes.len());
            bytes.splice(at..at, chunk);
        }
    }
    bytes
}

/// Every file under `dir`, in a stable order.
pub fn documents(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(documents(&path));
        } else {
            found.push(path);
        }
    }
    found.sort();
    found
}

/// The test documents and the quality corpus, by file name.
pub fn corpus() -> Vec<(String, Vec<u8>)> {
    let mut found: Vec<PathBuf> = documents(Path::new(DOCUMENTS));
    found.extend(documents(Path::new("../quality/corpus")));
    found
        .into_iter()
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            (name, fs::read(&path).unwrap())
        })
        .collect()
}

/// The resident memory of a process and every process below it, from /proc.
#[cfg(target_os = "linux")]
pub fn tree_rss_bytes(pid: u32) -> u64 {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let own = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| {
            value
                .trim()
                .trim_end_matches(" kB")
                .trim()
                .parse::<u64>()
                .ok()
        })
        .map_or(0, |kib| kib * 1024);
    let children: u64 = fs::read_dir(format!("/proc/{pid}/task"))
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|task| {
            fs::read_to_string(task.path().join("children"))
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter_map(|child| child.parse().ok())
        .map(tree_rss_bytes)
        .sum();
    own + children
}
