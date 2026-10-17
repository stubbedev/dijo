#![cfg(target_os = "linux")]

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

mod common;

use common::{text_pdf, write_docx, Scratch, StoredZip, XorShift};

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
    /// The metadata, as Tika's keys and values.
    metadata: String,
}

/// Extracts `path`, counting the output without keeping it.
fn extract(path: &Path) -> Extraction {
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_extractous"))
        .arg("--metadata")
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let metadata = std::thread::spawn(move || {
        let mut metadata = String::new();
        stderr.read_to_string(&mut metadata).unwrap();
        metadata
    });
    let mut stdout = child.stdout.take().unwrap();
    let mut first = [0u8; 1];
    let output = stdout.read(&mut first).unwrap() as u64;
    let first_output = started.elapsed();
    let output = output + io::copy(&mut stdout, &mut io::sink()).unwrap();
    let status = child.wait().unwrap();
    let metadata = metadata.join().unwrap();
    assert!(status.success(), "{metadata}");
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
        metadata,
    }
}

/// Asserts the text of `path` came out streamed and in bounded memory: at least `min_output`
/// bytes of it, the first before half the extraction had passed, and peak memory under a fixed
/// bound whatever the input's size. With a native `parser`, the input was extracted by it.
fn assert_bounded(path: &Path, min_output: u64, parser: Option<&str>) {
    let input = fs::metadata(path).unwrap().len();
    let extraction = extract(path);
    eprintln!(
        "{}: {} MiB of input, {} MiB of text, peak RSS {} MiB, {:?}",
        path.display(),
        input >> 20,
        extraction.output >> 20,
        extraction.peak_rss >> 20,
        extraction.elapsed
    );
    if let Some(parser) = parser {
        assert!(
            extraction
                .metadata
                .contains(&format!("extractous::{parser}")),
            "{} was not extracted by the native {parser}: {}",
            path.display(),
            extraction.metadata
        );
    }
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
    assert_bounded(&path, input / 2, Some("TextAndCSVParser"));
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn large_utf16_text_extracts_in_bounded_memory() {
    let scratch = Scratch::new("utf16");
    let path = scratch.0.join("large-utf16.txt");
    let mut file = BufWriter::new(File::create(&path).unwrap());
    file.write_all(b"\xFF\xFE").unwrap();
    let mut written = 2u64;
    let mut index = 0u64;
    while written < 512 << 20 {
        let line = format!("{index} Smørrebrød på Østerbro, blåbærgrød og æbleskiver\n");
        let units: Vec<u8> = line.encode_utf16().flat_map(u16::to_le_bytes).collect();
        file.write_all(&units).unwrap();
        written += units.len() as u64;
        index += 1;
    }
    file.flush().unwrap();
    assert_bounded(&path, written / 3, Some("TextAndCSVParser"));
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
    assert_bounded(&path, input / 2, Some("TextAndCSVParser"));
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
    assert_bounded(&path, input / 2, Some("MboxParser"));
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn a_pdf_of_many_pages_extracts_in_bounded_memory() {
    let scratch = Scratch::new("pdf");
    let path = scratch.0.join("large.pdf");
    let pages = 50_000;
    fs::write(&path, text_pdf(pages)).unwrap();
    assert_bounded(&path, pages as u64 * 4_000, Some("PDFParser"));
}

/// A PDF being written object by object, with where each object starts.
struct PdfWriter {
    out: BufWriter<File>,
    offsets: Vec<usize>,
    written: usize,
}

impl PdfWriter {
    fn new(path: &Path) -> Self {
        let mut writer = Self {
            out: BufWriter::new(File::create(path).unwrap()),
            offsets: Vec::new(),
            written: 0,
        };
        writer.write(b"%PDF-1.7\n");
        writer
    }

    fn write(&mut self, bytes: &[u8]) {
        self.out.write_all(bytes).unwrap();
        self.written += bytes.len();
    }

    /// Writes the next object, numbered from 1.
    fn object(&mut self, body: &str) {
        self.offsets.push(self.written);
        let object = format!("{} 0 obj\n{body}\nendobj\n", self.offsets.len());
        self.write(object.as_bytes());
    }

    /// Writes the cross-reference table and the trailer, the first object the catalog.
    fn finish(mut self) {
        let xref = self.written;
        let mut table = format!("xref\n0 {}\n0000000000 65535 f \n", self.offsets.len() + 1);
        for offset in &self.offsets {
            table.push_str(&format!("{offset:010} 00000 n \n"));
        }
        table.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            self.offsets.len() + 1
        ));
        self.write(table.as_bytes());
        self.out.flush().unwrap();
    }
}

/// Writes a PDF of `pages` pages to `path`, each page with a content stream of its own of
/// about 10 KB, generated as it is written.
fn write_large_pdf(path: &Path, pages: usize) {
    let mut pdf = PdfWriter::new(path);
    pdf.object("<< /Type /Catalog /Pages 2 0 R >>");
    let kids: Vec<String> = (0..pages)
        .map(|page| format!("{} 0 R", 4 + 2 * page))
        .collect();
    pdf.object(&format!(
        "<< /Type /Pages /Kids [{}] /Count {pages} >>",
        kids.join(" ")
    ));
    pdf.object("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>");
    for page in 0..pages {
        let mut content = String::from("BT /F1 10 Tf 12 TL 40 800 Td");
        for line in 0..60 {
            content.push_str(&format!(
                " (Page {page}, line {line} of a long generated document, written to keep the \
                 parser busy for a while.) Tj T*"
            ));
        }
        content.push_str(" ET");
        pdf.object(&format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] \
             /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            5 + 2 * page
        ));
        pdf.object(&format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
    pdf.finish();
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn a_large_pdf_extracts_natively_in_bounded_memory() {
    let scratch = Scratch::new("large-pdf");
    let path = scratch.0.join("large.pdf");
    let pages = 60_000;
    write_large_pdf(&path, pages);
    assert!(fs::metadata(&path).unwrap().len() > 256 << 20);
    assert_bounded(&path, pages as u64 * 5_000, Some("PDFParser"));
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
    assert_bounded(&path, size / 3, Some("OOXMLParser"));
}

/// Writes a workbook of one sheet to `path`, the sheet's rows written by `rows`, as a deflated
/// entry when `deflated`.
fn write_xlsx(path: &Path, deflated: bool, rows: impl FnOnce(&mut dyn Write) -> io::Result<()>) {
    let sheet = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    let relationships = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let mut zip = StoredZip::new(BufWriter::new(File::create(path).unwrap()));
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
    let sheet_part = |out: &mut dyn Write| {
        write!(
            out,
            "<?xml version=\"1.0\"?><worksheet xmlns=\"{sheet}\"><sheetData>"
        )?;
        rows(out)?;
        write!(out, "</sheetData></worksheet>")
    };
    if deflated {
        zip.deflated_entry("xl/worksheets/sheet1.xml", sheet_part)
            .unwrap();
    } else {
        zip.entry("xl/worksheets/sheet1.xml", sheet_part).unwrap();
    }
    zip.finish().unwrap().flush().unwrap();
}

/// Writes `count` rows of a number and an inline string each.
fn ledger_rows(out: &mut dyn Write, count: u64) -> io::Result<()> {
    for row in 1..=count {
        write!(
            out,
            "<row r=\"{row}\"><c r=\"A{row}\"><v>{row}</v></c>\
             <c r=\"B{row}\" t=\"inlineStr\"><is><t>entry {row} of the ledger</t></is></c></row>"
        )?;
    }
    Ok(())
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn a_spreadsheet_of_a_million_rows_extracts_in_bounded_memory() {
    let scratch = Scratch::new("xlsx");
    let path = scratch.0.join("large.xlsx");
    let rows = 1_000_000;
    write_xlsx(&path, false, |out| ledger_rows(out, rows));
    assert_bounded(&path, rows * 20, Some("OOXMLParser"));
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn a_deflated_sheet_larger_than_memory_extracts_in_bounded_memory() {
    let scratch = Scratch::new("deflated-xlsx");
    let path = scratch.0.join("deflated.xlsx");
    let rows = 4_000_000;
    write_xlsx(&path, true, |out| ledger_rows(out, rows));
    assert_bounded(&path, rows * 20, Some("OOXMLParser"));
}

#[test]
#[ignore = "generates gigabytes; run with `just test-large`"]
fn a_large_archive_of_deflated_entries_extracts_in_bounded_memory() {
    let scratch = Scratch::new("deflated-zip");
    let path = scratch.0.join("deflated.zip");
    let words = [
        "keeper", "ledger", "lantern", "harbour", "storm", "rope", "gull", "tide", "beacon",
        "island", "fog", "anchor", "mast", "pier", "wave", "salt",
    ];
    let mut random = XorShift(0x5EED);
    let mut zip = StoredZip::new(BufWriter::new(File::create(&path).unwrap()));
    let mut text = 0u64;
    for entry in 0..300 {
        zip.deflated_entry(&format!("entry{entry:03}.txt"), |out| {
            let mut line = String::new();
            for _ in 0..40_000 {
                line.clear();
                for _ in 0..12 {
                    line.push_str(words[random.below(words.len())]);
                    line.push(' ');
                }
                line.push_str(&random.next().to_string());
                line.push('\n');
                out.write_all(line.as_bytes())?;
                text += line.len() as u64;
            }
            Ok(())
        })
        .unwrap();
    }
    zip.finish().unwrap().flush().unwrap();
    assert!(fs::metadata(&path).unwrap().len() > 64 << 20);
    assert_bounded(&path, text * 9 / 10, Some("ZipParser"));
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
    assert_bounded(
        &inner.unwrap(),
        10 * level_text.len() as u64 * 9 / 10,
        Some("ZipParser"),
    );
}
