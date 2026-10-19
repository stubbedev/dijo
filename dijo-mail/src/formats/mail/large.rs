//! Messages and mailboxes too large to read whole, extracted under a memory budget far below
//! their size: a part's body is decoded a chunk at a time to a spool, straight to a file where
//! its encoded length says it is too large to hold in memory.

use std::io::{self, Write};

use dijo_core::large::{write_base64, Binary, LargeFile, BUDGET, END};
use dijo_core::Extractor;

use crate::input::MAX_READ_WHOLE;

/// Writes to `out` a message whose lines end in `line_end`, whose first part is a binary
/// attachment of `large` bytes in base64, in lines of 76 characters, and whose second is the
/// text [`END`].
fn message_of_large_attachment(out: &mut dyn Write, large: u64, line_end: &str) -> io::Result<()> {
    let head = [
        "From: a@example.com",
        "Subject: large",
        "MIME-Version: 1.0",
        "Content-Type: multipart/mixed; boundary=b",
        "",
        "--b",
        "Content-Type: application/octet-stream",
        "Content-Transfer-Encoding: base64",
        "Content-Disposition: attachment; filename=large.bin",
        "",
    ];
    for line in head {
        write!(out, "{line}{line_end}")?;
    }
    write_base64(out, Binary::new(large), line_end.as_bytes())?;
    for line in [
        "--b",
        "Content-Type: text/plain",
        "Content-Disposition: attachment; filename=end.txt",
        "",
        END,
        "--b--",
    ] {
        write!(out, "{line}{line_end}")?;
    }
    Ok(())
}

#[test]
fn decodes_an_attachment_too_large_to_read_whole_to_a_spool() {
    let file = LargeFile::written("large.eml", |out| {
        message_of_large_attachment(out, MAX_READ_WHOLE, "\r\n")
    });
    let (text, _) = file.extract_with(&Extractor::new(), BUDGET, "dijo::RFC822Parser");
    assert!(text.contains(END), "{text}");
}

/// A message whose HTML body is too large to read whole is written as its DOM is built, from a
/// spool, rather than built whole: the comments that fill it write nothing but the line breaks
/// between them, more than a string extraction takes by default, and the paragraph after them
/// is written.
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
    let whole = Extractor::new().set_extract_string_max_length(i32::MAX);
    let (text, _) = file.extract_with(&whole, BUDGET, "dijo::RFC822Parser");
    assert!(
        text.contains(END),
        "{}",
        &text[text.len().saturating_sub(200)..]
    );
}

/// A mailbox of messages of a mebibyte's attachment each, too large to read whole, is read a
/// message at a time, each handed on in place.
#[test]
fn reads_a_mailbox_too_large_to_read_whole_a_message_at_a_time() {
    const ATTACHMENT: u64 = 1 << 20;
    let messages = MAX_READ_WHOLE / ATTACHMENT + 1;
    let file = LargeFile::written("large.mbox", |out| {
        for _ in 0..messages {
            out.write_all(b"From a@example.com Mon Jan  1 00:00:00 2024\n")?;
            message_of_large_attachment(out, ATTACHMENT, "\n")?;
        }
        Ok(())
    });
    let (text, _) = file.extract_with(&Extractor::new(), BUDGET, "dijo::MboxParser");
    assert_eq!(text.matches(END).count() as u64, messages);
}
