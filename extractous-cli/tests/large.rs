#![cfg(target_os = "linux")]

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod common;

use common::{text_pdf, write_docx, Scratch, StoredZip};

const MAX_RSS_BYTES: u64 = 512 << 20;

fn generate(path: &Path, size: u64, header: &str, mut record: impl FnMut(u64) -> String) -> u64 {
    let mut file = BufWriter::new(File::create(path).unwrap());
    file.write_all(header.as_bytes()).unwrap();
    let mut written = header.len() as u64;
    let mut index = 0;
    while written < size {
        let line = record(index);
        file.write_all(line.as_bytes()).unwrap();
        written += line.len() as u64;
        index += 1;
    }
    file.flush().unwrap();
    written
}

/// What extracting one large input produced.
struct Extraction {
    output: u64,
    peak_rss: u64,
    first_output: Duration,
    elapsed: Duration,
}

/// Extracts `path`, counting the output without keeping it.
fn extract(path: &Path) -> Extraction {
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_extractous"))
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut first = [0u8; 1];
    let output = stdout.read(&mut first).unwrap() as u64;
    let first_output = started.elapsed();
    let output = output + io::copy(&mut stdout, &mut io::sink()).unwrap();
    assert!(child.wait().unwrap().success());
    let elapsed = started.elapsed();
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) },
        0
    );
    Extraction {
        output,
        peak_rss: usage.ru_maxrss as u64 * 1024,
        first_output,
        elapsed,
    }
}

/// Asserts the text of `path` came out streamed and in bounded memory: at least `min_output`
/// bytes of it, the first before half the extraction had passed, and peak memory under a fixed
/// bound whatever the input's size.
fn assert_bounded(path: &Path, min_output: u64) {
    let input = fs::metadata(path).unwrap().len();
    let extraction = extract(path);
    assert!(
        extraction.output >= min_output,
        "{} bytes of text from {input} bytes of input",
        extraction.output
    );
    assert!(
        extraction.peak_rss < MAX_RSS_BYTES,
        "peak RSS {} MiB for {} MiB of input",
        extraction.peak_rss >> 20,
        input >> 20
    );
    assert!(
        extraction.first_output < extraction.elapsed / 2,
        "the first output came after {:?} of {:?}",
        extraction.first_output,
        extraction.elapsed
    );
}

/// Extracts the metadata of `path` as JSON with Tika's raw keys, returning the output and the
/// peak resident memory of the extraction.
fn extract_metadata(path: &Path) -> (String, u64) {
    let output = Command::new(env!("CARGO_BIN_EXE_extractous"))
        .args(["--format", "json", "--raw-metadata"])
        .arg(path)
        .stderr(Stdio::inherit())
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) },
        0
    );
    (
        String::from_utf8(output.stdout).unwrap(),
        usage.ru_maxrss as u64 * 1024,
    )
}

/// A file of `length` bytes that starts with `head`, the rest a hole that reads as zeros.
fn sparse(path: &Path, head: &[u8], length: u64) {
    fs::write(path, head).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_len(length)
        .unwrap();
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn a_long_recording_is_read_natively_and_only_in_part() {
    let scratch = Scratch::new("recording");
    let path = scratch.0.join("long.wav");
    let samples: u32 = 3 << 30;
    let mut wave = b"RIFF".to_vec();
    wave.extend((36 + samples).to_le_bytes());
    wave.extend(b"WAVEfmt ");
    for field in [16u32, 0x0002_0001, 44_100, 44_100 * 4, 0x0010_0004] {
        wave.extend(field.to_le_bytes());
    }
    wave.extend(b"data");
    wave.extend(samples.to_le_bytes());
    let length = wave.len() as u64 + u64::from(samples);
    sparse(&path, &wave, length);
    let (output, peak_rss) = extract_metadata(&path);
    assert!(output.contains("extractous::AudioParser"), "{output}");
    assert!(output.contains("\"xmpDM:audioSampleRate\""), "{output}");
    assert!(
        peak_rss < MAX_RSS_BYTES,
        "peak RSS {} MiB for {} MiB of input",
        peak_rss >> 20,
        length >> 20
    );
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn large_text_extracts_in_bounded_memory() {
    let scratch = Scratch::new("text");
    let path = scratch.0.join("large.txt");
    let input = generate(&path, 1 << 30, "", |index| {
        format!("{index} the quick brown fox jumps over the lazy dog again and again\n")
    });
    assert_bounded(&path, input / 2);
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn large_csv_extracts_in_bounded_memory() {
    let scratch = Scratch::new("csv");
    let path = scratch.0.join("large.csv");
    let input = generate(&path, 512 << 20, "id,name,value,comment\n", |index| {
        format!(
            "{index},name {index},{}.{:02},\"a comment about row {index}\"\n",
            index * 3,
            index % 100
        )
    });
    assert_bounded(&path, input / 2);
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn large_mbox_extracts_in_bounded_memory() {
    let scratch = Scratch::new("mbox");
    let path = scratch.0.join("large.mbox");
    let body = "The quick brown fox jumps over the lazy dog.\n".repeat(40);
    let input = generate(&path, 256 << 20, "", |index| {
        format!(
            "From sender{index}@example.com Mon Jan  1 00:00:00 2024\n\
             From: sender{index}@example.com\nTo: recipient@example.com\n\
             Subject: message {index}\nDate: Mon, 1 Jan 2024 00:00:00 +0000\n\n{body}\n"
        )
    });
    assert_bounded(&path, input / 2);
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn a_pdf_of_many_pages_extracts_in_bounded_memory() {
    let scratch = Scratch::new("pdf");
    let path = scratch.0.join("large.pdf");
    let pages = 50_000;
    fs::write(&path, text_pdf(pages)).unwrap();
    assert_bounded(&path, pages as u64 * 4_000);
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn a_large_docx_extracts_in_bounded_memory() {
    let scratch = Scratch::new("docx");
    let path = scratch.0.join("large.docx");
    let paragraph =
        "The keeper climbed the hundred and twelve steps and wrote one line in the ledger.";
    let size = 512u64 << 20;
    let out = BufWriter::new(File::create(&path).unwrap());
    write_docx(out, "", |out| {
        let mut written = 0;
        while written < size {
            let run = format!("<w:p><w:r><w:t>{paragraph}</w:t></w:r></w:p>");
            out.write_all(run.as_bytes())?;
            written += run.len() as u64;
        }
        Ok(())
    })
    .unwrap()
    .flush()
    .unwrap();
    assert_bounded(&path, size / 3);
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn a_spreadsheet_of_a_million_rows_extracts_in_bounded_memory() {
    let scratch = Scratch::new("xlsx");
    let path = scratch.0.join("large.xlsx");
    let rows = 1_000_000;
    let sheet = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    let relationships = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let mut zip = StoredZip::new(BufWriter::new(File::create(&path).unwrap()));
    let parts = [
        (
            "[Content_Types].xml",
            "<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
             <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
             <Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
             <Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/></Types>"
                .to_string(),
        ),
        (
            "_rels/.rels",
            format!(
                "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
                 <Relationship Id=\"rId1\" Type=\"{relationships}/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>"
            ),
        ),
        (
            "xl/workbook.xml",
            format!(
                "<?xml version=\"1.0\"?><workbook xmlns=\"{sheet}\" xmlns:r=\"{relationships}\">\
                 <sheets><sheet name=\"Rows\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>"
            ),
        ),
        (
            "xl/_rels/workbook.xml.rels",
            format!(
                "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
                 <Relationship Id=\"rId1\" Type=\"{relationships}/worksheet\" Target=\"worksheets/sheet1.xml\"/></Relationships>"
            ),
        ),
    ];
    for (name, content) in &parts {
        zip.entry(name, |out| out.write_all(content.as_bytes()))
            .unwrap();
    }
    zip.entry("xl/worksheets/sheet1.xml", |out| {
        write!(
            out,
            "<?xml version=\"1.0\"?><worksheet xmlns=\"{sheet}\"><sheetData>"
        )?;
        for row in 1..=rows {
            write!(
                out,
                "<row r=\"{row}\"><c r=\"A{row}\"><v>{row}</v></c>\
                 <c r=\"B{row}\" t=\"inlineStr\"><is><t>entry {row} of the ledger</t></is></c></row>"
            )?;
        }
        write!(out, "</sheetData></worksheet>")
    })
    .unwrap();
    zip.finish().unwrap().flush().unwrap();
    assert_bounded(&path, rows * 20);
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn archives_nested_nine_deep_extract_in_bounded_memory() {
    let scratch = Scratch::new("nested");
    let level_text = "A line of text at one level of a deeply nested archive.\n".repeat(300_000);
    let mut inner: Option<std::path::PathBuf> = None;
    for level in 1..=9 {
        let path = scratch.0.join(format!("level{level}.zip"));
        let mut zip = StoredZip::new(BufWriter::new(File::create(&path).unwrap()));
        zip.entry(&format!("level{level}.txt"), |out| {
            out.write_all(level_text.as_bytes())
        })
        .unwrap();
        if let Some(inner) = &inner {
            zip.entry("inner.zip", |out| {
                io::copy(&mut File::open(inner)?, out).map(|_| ())
            })
            .unwrap();
        }
        zip.finish().unwrap().flush().unwrap();
        inner = Some(path);
    }
    assert_bounded(&inner.unwrap(), 10 * level_text.len() as u64 * 9 / 10);
}
