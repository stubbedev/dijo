//! The vocabulary of the parity comparisons between the native parsers and Tika's path, which
//! the harness in `extractous-core` runs: what the tests of a format name where their parser
//! departs from Tika's path on purpose, and the rewrites that undo such a departure.

/// What a native parser does otherwise than Tika's path on purpose, where Tika is wrong, with
/// the issue that tells why. The parser's own tests check what it does instead.
#[cfg(feature = "tika")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Departure<'a> {
    /// The values of a metadata name, not compared, in the metadata nor in the XHTML head.
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
    /// jsoup does.
    BlankLines(&'a str),
    /// The PNG files of the embedded documents a spool holds: compared by the image each holds,
    /// its size, colour type, depth, resolution and pixels, not by how it is compressed.
    PngImages(&'a str),
}

/// What a native parser does otherwise than Tika's path on purpose.
#[cfg(feature = "tika")]
pub type Departures<'a> = &'a [Departure<'a>];

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
