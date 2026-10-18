//! Tika's `DataURISchemeUtil`: a data URI read from a link's destination, or each one found in
//! HTML, ended by a quote, and the document it holds.

use std::sync::LazyLock;

use regex::{Captures, Regex};

use crate::formats::thrown::{Exception, Thrown};
use crate::java;
use crate::mime::MediaType;
use crate::text::charset::Charset;

/// `PARSE_PATTERN`.
static PARSE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)data:([^,]*?)(base64)?,(.*)$").expect("the pattern is valid")
});

/// `EXTRACT_PATTERN`.
static EXTRACT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)data:([^,]*?)(base64)?,([^"']*)["']"#).expect("the pattern is valid")
});

/// A data URI: its media type as written, whether its data is in base 64, and its data, each
/// backslash a space, as `DataURISchemeUtil.build` takes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DataUri {
    media_type: String,
    base64: bool,
    data: String,
}

/// `DataURISchemeUtil.parse`: the data URI `destination` holds, or none, where Tika's parse
/// throws the `DataURISchemeParseException` the Markdown parser ignores.
pub(super) fn parse(destination: &str) -> Option<DataUri> {
    PARSE.captures(destination).map(|found| DataUri::of(&found))
}

/// `DataURISchemeUtil.extract`: each data URI in `html` that a quote ends.
pub(super) fn extract(html: &str) -> Vec<DataUri> {
    EXTRACT
        .captures_iter(html)
        .map(|found| DataUri::of(&found))
        .collect()
}

/// The charsets `build` encodes text data in that this port encodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Utf8,
    Ascii,
    Latin1,
}

impl DataUri {
    fn of(found: &Captures) -> Self {
        Self {
            media_type: found[1].to_owned(),
            base64: found.get(2).is_some(),
            data: found[3].replace('\\', " "),
        }
    }

    /// `getMediaType().toString()`, where the media type parses.
    pub(super) fn content_type(&self) -> Option<String> {
        MediaType::parse(&self.media_type).map(|parsed| parsed.to_string())
    }

    /// Why the native parser leaves the document to Tika: text data in a charset this port does
    /// not encode.
    pub(super) fn unported(&self) -> Option<String> {
        match self.encoding() {
            Ok(Err(charset)) => Some(format!(
                "a data URI of text in {charset}, which the native parser does not encode"
            )),
            Ok(Ok(_)) | Err(_) => None,
        }
    }

    /// The data `build` takes: none, base 64 decoded as commons-codec decodes it, or text in the
    /// charset its media type names, UTF-8 where it names none or one Java does not know; or what
    /// `build` throws on a media type that does not parse or a charset name Java rejects.
    pub(super) fn bytes(&self) -> Result<Vec<u8>, Thrown> {
        if self.data.is_empty() {
            return Ok(Vec::new());
        }
        if self.base64 {
            return Ok(java::commons_base64_decode(&self.data));
        }
        let encoding = self.encoding()?.unwrap_or_else(|charset| {
            unreachable!("text in {charset} is left to Tika before it is written")
        });
        let replaced = |limit: u32| -> Vec<u8> {
            self.data
                .chars()
                .map(|character| {
                    u8::try_from(u32::from(character))
                        .ok()
                        .filter(|&byte| u32::from(byte) <= limit)
                        .unwrap_or(b'?')
                })
                .collect()
        };
        Ok(match encoding {
            Encoding::Utf8 => self.data.as_bytes().to_vec(),
            Encoding::Ascii => replaced(0x7f),
            Encoding::Latin1 => replaced(0xff),
        })
    }

    /// The encoding of text data: the charset its media type names, if Java knows it, else UTF-8;
    /// the name of one this port does not encode; or what `build` throws.
    fn encoding(&self) -> Result<Result<Encoding, String>, Thrown> {
        if self.base64 || self.data.is_empty() {
            return Ok(Ok(Encoding::Utf8));
        }
        let media_type = MediaType::parse(&self.media_type)
            .ok_or_else(|| Thrown::Runtime(Exception::bare("java.lang.NullPointerException")))?;
        let Some(name) = media_type.parameter("charset") else {
            return Ok(Ok(Encoding::Utf8));
        };
        if !is_legal_charset_name(name) {
            return Err(Thrown::Runtime(Exception::new(
                "java.nio.charset.IllegalCharsetNameException",
                name,
            )));
        }
        let Some(charset) = Charset::for_name(name) else {
            return Ok(Err(name.to_owned()));
        };
        Ok(match charset.name() {
            "UTF-8" => Ok(Encoding::Utf8),
            "US-ASCII" => Ok(Encoding::Ascii),
            "ISO-8859-1" => Ok(Encoding::Latin1),
            other => Err(other.to_owned()),
        })
    }
}

/// `Charset.checkName`: not empty, of ASCII letters, digits, and after the first character also
/// `-`, `+`, `:`, `_` and `.`.
fn is_legal_charset_name(name: &str) -> bool {
    !name.is_empty()
        && name.chars().enumerate().all(|(at, character)| {
            character.is_ascii_alphanumeric()
                || (at > 0 && matches!(character, '-' | '+' | ':' | '_' | '.'))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_data_uris_as_tika_does() {
        let gif = parse("data:image/gif;base64,R0lG").unwrap();
        assert_eq!(gif.content_type().as_deref(), Some("image/gif"));
        assert_eq!(gif.bytes().unwrap(), b"GIF");
        let text = parse("data:text/plain;charset=ISO-8859-1,K\u{f8}b\\enhavn \u{2603}").unwrap();
        assert_eq!(text.bytes().unwrap(), b"K\xf8b enhavn ?");
        assert_eq!(parse("data:,").unwrap().bytes().unwrap(), b"");
        assert!(parse("data:text").is_none());
        assert!(matches!(
            parse("data:,hej").unwrap().bytes(),
            Err(Thrown::Runtime(_))
        ));
        assert!(matches!(
            parse("data:text/plain;charset=-x,hej").unwrap().bytes(),
            Err(Thrown::Runtime(_))
        ));
        assert!(parse("data:text/plain;charset=windows-1252,hej")
            .unwrap()
            .unported()
            .is_some());
        let found = extract("<img src=\"data:image/png;base64,AAAA\"> data:x/y,z' data:a/b,c");
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[1].data, "z");
    }
}
