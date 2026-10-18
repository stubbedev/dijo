//! The vocabulary of the parity comparisons between the native parsers and Tika's path, which
//! the harness in `extractous-core` runs: what the tests of a format name where their parser
//! departs from Tika's path on purpose, and the rewrites that undo such a departure.

/// What a native parser does otherwise than Tika's path on purpose, where Tika is wrong, with
/// the issue that tells why. The parser's own tests check what it does instead.
#[cfg(feature = "tika")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Departure<'a> {
    /// The values of a metadata name, not compared, in the metadata nor in the XHTML head, nor
    /// in the metadata how many there are.
    Values(&'a str, &'a str),
    /// A metadata name Tika's path sets wrongly and the native parser does not set: left out of
    /// both results, in the metadata and as each `meta` element of the XHTML head.
    Unset(&'a str, &'a str),
    /// Line breaks: the text is compared with each CRLF an LF, where Tika's path makes a line
    /// feed a CRLF.
    LineBreaks(&'a str),
    /// Blank lines: the text is compared with each run of white space that holds a line break
    /// (LF or CR) one LF, where the native HTML parser puts the white space around a document's
    /// head and body, and its line breaks, as the WHATWG algorithm says rather than as Tika's
    /// jsoup does, or where blocks held in a paragraph are written before the element they sit
    /// in closes, the line breaks after them with them ([`ANCHORED_BLOCKS`]).
    BlankLines(&'a str),
    /// Elements: the body of the XHTML output is compared as its text, without its tags, where
    /// the native output builds another tree around the same text than Tika's path does: the
    /// native HTML parser, as the WHATWG algorithm says, with a table inside a paragraph of a
    /// quirks mode document or the line feed that starts a `pre` left out, or blocks held in a
    /// paragraph written inside the element they sit in ([`ANCHORED_BLOCKS`]). The text output
    /// is compared in full.
    Elements(&'a str),
    /// Links: the HTML parser resolves links as the WHATWG URL standard does, where Tika's path
    /// resolves them as `java.net.URL` does. Each value of an XHTML attribute that holds a
    /// resolved link, and each `Content-Location`, is compared as Tika's as the standard
    /// resolves it against the native `Content-Location`, where that is the native one.
    Links(&'a str),
    /// The PNG files of the embedded documents a spool holds: compared by the image each holds,
    /// its size, colour type, depth, resolution and pixels, not by how it is compressed.
    PngImages(&'a str),
    /// Placeable headers: the WMF files of the embedded documents a spool holds, compared with
    /// the units per inch of their placeable header, and its checksum, as POI writes them, 72
    /// whatever the picture's units, where the native parser gives each picture its own.
    PlaceableHeaders(&'a str),
    /// Whole content: the embedded documents a spool holds, where the native parser keeps every
    /// byte of a document cut short that Tika's path loses the end of. Each of Tika's files is
    /// compared with as much of the start of the native one, which must not be shorter.
    WholeContent(&'a str),
    /// Paragraph order: the text is compared as the set of its lines, each of Tika's one that the
    /// native text has too, but for the paragraphs the second field names, which Tika's path
    /// writes from earlier revisions of the document. The native parser writes each paragraph
    /// once, in the order of the document, and those Tika's path misses as well. A string the
    /// length limit cuts holds other paragraphs, so its text is not compared, nor the metadata
    /// only one of the two has where the limit stopped one before the end of the document. What
    /// OCR read of the embedded images is compared in any order too: each read of the native
    /// parser is one of Tika's path's, which also reads the images of earlier revisions. So are
    /// the files a spool holds: each of the native parser's, by its content, one of Tika's
    /// path's, but for those a budget cut, which hold the start of other files.
    Paragraphs(&'a str, &'a [&'a str]),
}

/// What a native parser does otherwise than Tika's path on purpose.
#[cfg(feature = "tika")]
pub type Departures<'a> = &'a [Departure<'a>];

/// Where a document has a block held in a paragraph that sits in another element of the
/// paragraph, such as a table's later rows or an embedded document's text in its
/// `package-entry`: the native handler writes it before that element closes, where Tika's
/// `AnchoredBlockHandler` moves it after the paragraph, out of the element (#136).
#[cfg(feature = "tika")]
pub const ANCHORED_BLOCKS: Departures<'static> =
    &[Departure::BlankLines("#136"), Departure::Elements("#136")];

/// The documents of Tika's tests and of the repository's corpora that depart by
/// [`ANCHORED_BLOCKS`], by file name: a table in a paragraph, notes of several paragraphs, and
/// attachments and embedded objects in paragraphs.
#[cfg(feature = "tika")]
pub const ANCHORED_BLOCK_DOCUMENTS: [&str; 7] = [
    "testPages.pages",
    "testWORD_endnote_table.docx",
    "testWORD_numbered_list.docx",
    "testWORD_2003ml.xml",
    "test-incremental-updates.eml",
    "testRFC822_normal_zip",
    "nested.zip",
];

/// The metadata of an extraction, as the two paths are compared on it.
#[cfg(feature = "tika")]
pub type ComparedMetadata = Vec<(String, Vec<String>)>;

/// A rewrite of a native result, its decoded text and its metadata, into what Tika's path
/// writes, given Tika's metadata, undoing a fix that departs from Tika's path systematically:
/// it names the issue of the quirk, and undoes only what that fix changes.
#[cfg(feature = "tika")]
pub type NativeRewrite = fn(&mut String, &mut ComparedMetadata, &ComparedMetadata);

/// For a [`NativeRewrite`]: the native result without the metadata `name`, in its metadata and
/// as each `meta` element of the XHTML head, the one a length limit cut included.
#[cfg(feature = "tika")]
pub fn without_metadata(text: &mut String, metadata: &mut ComparedMetadata, name: &str) {
    metadata.retain(|(native_name, _)| native_name != name);
    let meta = format!("<meta name=\"{name}\" content=\"");
    while let Some(start) = text.find(&meta) {
        let end = text[start..]
            .find("\" />")
            .map_or(text.len(), |end| start + end + "\" />".len());
        let end = end + usize::from(text[end..].starts_with('\n'));
        text.replace_range(start..end, "");
    }
}

/// For a [`NativeRewrite`]: the native result with the values Tika's path gives the metadata
/// `name` in place of its own, in its metadata and in the content of each `meta` element of the
/// XHTML head, which must be as many and hold no characters XML escapes.
#[cfg(feature = "tika")]
pub fn with_tikas_values(
    text: &mut String,
    metadata: &mut ComparedMetadata,
    tika: &ComparedMetadata,
    name: &str,
) {
    let Some((_, values)) = tika.iter().find(|(tika_name, _)| tika_name == name) else {
        return without_metadata(text, metadata, name);
    };
    let Some((_, native)) = metadata
        .iter_mut()
        .find(|(native_name, _)| native_name == name)
    else {
        return;
    };
    assert_eq!(native.len(), values.len(), "the values of {name}");
    native.clone_from(values);
    let meta = format!("<meta name=\"{name}\" content=\"");
    let mut at = 0;
    for value in values {
        let Some(found) = text[at..].find(&meta) else {
            return;
        };
        let start = at + found + meta.len();
        let end = text[start..]
            .find('"')
            .map_or(text.len(), |end| start + end);
        text.replace_range(start..end, value);
        at = start + value.len();
    }
}

/// A rewrite of the decoded text of both results, for a fix whose departure cannot be undone
/// from the native result alone: it names the issue, and normalises only what that fix changes.
#[cfg(feature = "tika")]
pub type Normalisation = fn(&mut String);
