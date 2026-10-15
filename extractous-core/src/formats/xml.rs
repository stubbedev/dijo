//! Reading XML documents as Java's SAX parser reads them: decoded in the charset their byte order
//! mark or XML declaration names, by the JDK's own decoders, and failing on what it rejects.

use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::BytesRef;

use crate::formats::Failure;
use crate::text::charset::Charset;

const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
const UTF16_BE_BOM: &[u8] = &[0xFE, 0xFF];
const UTF16_LE_BOM: &[u8] = &[0xFF, 0xFE];

/// The text of an XML document: what follows its byte order mark, decoded in the charset the
/// mark or the declaration names, else UTF-8. `what` names the document in errors.
pub(crate) fn decode(input: &[u8], what: &str) -> Result<String, Failure> {
    let malformed = |reason: String| Failure::Malformed(format!("{what} {reason}"));
    if let Some(rest) = input.strip_prefix(UTF8_BOM) {
        return utf8(rest).map_err(malformed);
    }
    for (bom, name) in [(UTF16_BE_BOM, "UTF-16BE"), (UTF16_LE_BOM, "UTF-16LE")] {
        if let Some(rest) = input.strip_prefix(bom) {
            return decode_in(Charset::named(name), rest).map_err(malformed);
        }
    }
    match declared_encoding(input) {
        None => utf8(input).map_err(malformed),
        Some(label) => {
            let charset = Charset::for_name(&label).ok_or_else(|| {
                malformed(format!(
                    "declares the encoding {label:?}, which Java does not know"
                ))
            })?;
            decode_in(charset, input).map_err(malformed)
        }
    }
}

fn utf8(input: &[u8]) -> Result<String, String> {
    String::from_utf8(input.to_vec()).map_err(|e| format!("is not valid UTF-8: {}", e.utf8_error()))
}

fn decode_in(charset: Charset, input: &[u8]) -> Result<String, String> {
    if charset.name() == "UTF-8" {
        return utf8(input);
    }
    charset
        .decode(input)
        .ok_or_else(|| format!("is in {}, which extractous cannot decode", charset.name()))
}

/// The `encoding` of an XML declaration at the very start of `input`, which an ASCII-compatible
/// charset spells in ASCII.
fn declared_encoding(input: &[u8]) -> Option<String> {
    let declaration = input.strip_prefix(b"<?xml")?;
    let end = declaration.windows(2).position(|pair| pair == b"?>")?;
    let declaration = std::str::from_utf8(&declaration[..end]).ok()?;
    let after = &declaration[declaration.find("encoding")? + "encoding".len()..];
    let after = after.trim_start().strip_prefix('=')?.trim_start();
    let quote = after
        .chars()
        .next()
        .filter(|quote| matches!(quote, '"' | '\''))?;
    let value = &after[1..];
    Some(value[..value.find(quote)?].to_owned())
}

/// The text an entity or character reference stands for: a character reference or one of XML's
/// five predefined entities. Any other entity was never declared, which Java's parser rejects.
pub(crate) fn resolve_reference(reference: &BytesRef, what: &str) -> Result<String, Failure> {
    let malformed = |reason: String| Failure::Malformed(format!("{what} {reason}"));
    if let Some(character) = reference
        .resolve_char_ref()
        .map_err(|e| malformed(format!("has a bad character reference: {e}")))?
    {
        return Ok(character.to_string());
    }
    let name = reference.xml10_content();
    resolve_predefined_entity(&name)
        .map(str::to_owned)
        .ok_or_else(|| malformed(format!("uses the entity &{name}; without declaring it")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_in_the_declared_charset() {
        let cyrillic = b"<?xml version=\"1.0\" encoding='windows-1251'?><a>\xcc\xe0\xff\xea</a>";
        assert_eq!(
            decode(cyrillic, "x").unwrap(),
            "<?xml version=\"1.0\" encoding='windows-1251'?><a>Маяк</a>"
        );
        let latin1 = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>\x80";
        assert!(decode(latin1, "x").unwrap().ends_with('\u{80}'));
    }

    #[test]
    fn decodes_byte_order_marks_and_defaults_to_strict_utf8() {
        assert_eq!(decode(b"\xef\xbb\xbf<a/>", "x").unwrap(), "<a/>");
        assert_eq!(decode(b"\xff\xfe<\0a\0/\0>\0", "x").unwrap(), "<a/>");
        assert_eq!(decode("<a>æ</a>".as_bytes(), "x").unwrap(), "<a>æ</a>");
        assert!(decode(b"<a>\xe6</a>", "x").is_err());
        assert!(decode(b"<?xml version=\"1.0\" encoding=\"no-such\"?><a/>", "x").is_err());
    }
}
