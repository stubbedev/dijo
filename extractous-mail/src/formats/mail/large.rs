//! Messages and mailboxes too large to read whole, extracted under a memory budget far below
//! their size: a part's body is decoded a chunk at a time to a spool, straight to a file where
//! its encoded length says it is too large to hold in memory.

use std::io::{self, Write};

use extractous_core::large::{write_base64, LargeFile};
use extractous_core::Extractor;

use crate::formats::archive::large::{Binary, BUDGET, END};
use crate::input::MAX_READ_WHOLE;

/// Writes to `out` a message whose first part is a binary attachment of `large` bytes in
/// base64, in lines of 76 characters, and whose second is the text [`END`].
fn message_of_large_attachment(out: &mut dyn Write, large: u64) -> io::Result<()> {
    out.write_all(
        b"From: a@example.com\r\nSubject: large\r\nMIME-Version: 1.0\r\n\
          Content-Type: multipart/mixed; boundary=b\r\n\r\n--b\r\n\
          Content-Type: application/octet-stream\r\nContent-Transfer-Encoding: base64\r\n\
          Content-Disposition: attachment; filename=large.bin\r\n\r\n",
    )?;
    write_base64(out, Binary::new(large), b"\r\n")?;
    write!(
        out,
        "--b\r\nContent-Type: text/plain\r\nContent-Disposition: attachment; \
         filename=end.txt\r\n\r\n{END}\r\n--b--\r\n"
    )
}

#[test]
fn decodes_an_attachment_too_large_to_read_whole_to_a_spool() {
    let file = LargeFile::written("large.eml", |out| {
        message_of_large_attachment(out, MAX_READ_WHOLE)
    });
    let (text, _) = file.extract_with(&Extractor::new(), BUDGET, "extractous::RFC822Parser");
    assert!(text.contains(END), "{text}");
}

/// A message whose HTML body is too large to read whole is written as its DOM is built, from a
/// spool, rather than built whole: the comments that fill it write nothing, and the paragraph
/// after them is written.
#[test]
fn writes_an_html_part_too_large_to_read_whole_as_it_is_built() {
    let file = LargeFile::written("large-html.eml", |out| {
        out.write_all(
            b"From: a@example.com\r\nSubject: large\r\nMIME-Version: 1.0\r\n\
              Content-Type: text/html; charset=utf-8\r\n\r\n<html><body>\r\n",
        )?;
        let line: &[u8] = b"<!-- a comment of the page, which its text leaves out -->\r\n";
        for _ in 0..(MAX_READ_WHOLE + (16 << 20)) / line.len() as u64 {
            out.write_all(line)?;
        }
        write!(out, "<p>{END}</p></body></html>\r\n")
    });
    let (text, _) = file.extract_with(&Extractor::new(), BUDGET, "extractous::RFC822Parser");
    assert!(text.contains(END), "{text}");
}

#[test]
fn reads_a_mailbox_too_large_to_read_whole_a_message_at_a_time() {
    let file = LargeFile::written("large.mbox", |out| {
        out.write_all(b"From a@example.com Mon Jan  1 00:00:00 2024\n")?;
        message_of_large_attachment(out, MAX_READ_WHOLE)
    });
    let (text, _) = file.extract_with(&Extractor::new(), BUDGET, "extractous::MboxParser");
    assert!(text.contains(END), "{text}");
}
