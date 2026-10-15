//! The objects of a document as PDFBox's COS layer reads them, over the objects `lopdf` parses:
//! an indirect reference resolves to its object, `null` and a missing object to nothing, a stream
//! is a dictionary too, and numbers, names and text strings convert as PDFBox converts them.

use lopdf::{Dictionary, Document, Object, ObjectId, Stream};

use super::Unported;
use crate::text::charset::Charset;

/// How many references in a row are followed before a chain counts as broken.
const MAX_REFERENCE_CHAIN: usize = 32;

/// The identity of an object, as PDFBox's identity-based sets tell objects apart: an indirect
/// object by its number, a direct one by where it is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum Identity {
    Indirect(ObjectId),
    Direct(usize),
    /// An object PDFBox creates afresh each time it is asked for, such as the font of a
    /// graphics state parameter dictionary: the count of those created before it.
    Unshared(usize),
}

impl Identity {
    fn of_direct(object: &Object) -> Self {
        Self::Direct(std::ptr::from_ref(object) as usize)
    }
}

/// A parsed document and its objects.
pub(super) struct Pdf {
    pub(super) doc: Document,
}

impl Pdf {
    /// The object `object` stands for: itself, or what its references lead to; nothing for
    /// `null`, a reference to a missing object or a chain of references too long to follow.
    pub(super) fn resolve<'a>(&'a self, object: &'a Object) -> Option<&'a Object> {
        self.resolve_identified(object).map(|(_, object)| object)
    }

    /// [`Pdf::resolve`], with the identity of the object it resolves to.
    pub(super) fn resolve_identified<'a>(
        &'a self,
        object: &'a Object,
    ) -> Option<(Identity, &'a Object)> {
        let mut identity = Identity::of_direct(object);
        let mut current = object;
        for _ in 0..MAX_REFERENCE_CHAIN {
            match current {
                Object::Reference(id) => {
                    identity = Identity::Indirect(*id);
                    current = self.doc.objects.get(id)?;
                }
                Object::Null => return None,
                _ => return Some((identity, current)),
            }
        }
        None
    }

    /// `COSDictionary.getDictionaryObject`: the resolved value of `key`.
    pub(super) fn get<'a>(&'a self, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
        dict.get(key).ok().and_then(|object| self.resolve(object))
    }

    /// The resolved value of `key` and its identity.
    pub(super) fn get_identified<'a>(
        &'a self,
        dict: &'a Dictionary,
        key: &[u8],
    ) -> Option<(Identity, &'a Object)> {
        dict.get(key)
            .ok()
            .and_then(|object| self.resolve_identified(object))
    }

    /// `COSDictionary.getCOSDictionary`: the value of `key` when it is a dictionary or a stream.
    pub(super) fn dict<'a>(&'a self, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Dictionary> {
        self.get(dict, key).and_then(as_dict)
    }

    /// `COSDictionary.getCOSStream`.
    pub(super) fn stream<'a>(&'a self, dict: &'a Dictionary, key: &[u8]) -> Option<&'a Stream> {
        match self.get(dict, key) {
            Some(Object::Stream(stream)) => Some(stream),
            _ => None,
        }
    }

    /// `COSDictionary.getCOSArray`.
    pub(super) fn array<'a>(&'a self, dict: &'a Dictionary, key: &[u8]) -> Option<&'a [Object]> {
        match self.get(dict, key) {
            Some(Object::Array(array)) => Some(array),
            _ => None,
        }
    }

    /// `COSDictionary.getCOSName`: the bytes of a name value.
    pub(super) fn name<'a>(&'a self, dict: &'a Dictionary, key: &[u8]) -> Option<&'a [u8]> {
        match self.get(dict, key) {
            Some(Object::Name(name)) => Some(name),
            _ => None,
        }
    }

    /// `COSDictionary.getInt(key, default)`.
    pub(super) fn int(&self, dict: &Dictionary, key: &[u8], default: i32) -> i32 {
        self.get(dict, key).and_then(int_value).unwrap_or(default)
    }

    /// `COSDictionary.getFloat(key, default)`.
    pub(super) fn float(&self, dict: &Dictionary, key: &[u8], default: f32) -> f32 {
        self.get(dict, key).and_then(float_value).unwrap_or(default)
    }

    /// `COSDictionary.getString`: a text string value as PDFBox decodes it.
    pub(super) fn text(&self, dict: &Dictionary, key: &[u8]) -> Option<String> {
        match self.get(dict, key) {
            Some(Object::String(bytes, _)) => Some(text_string(bytes)),
            _ => None,
        }
    }

    /// `COSDictionary.getNameAsString`: a name, or a text string.
    pub(super) fn name_as_string(&self, dict: &Dictionary, key: &[u8]) -> Option<String> {
        match self.get(dict, key) {
            Some(Object::Name(name)) => Some(name_string(name)),
            Some(Object::String(bytes, _)) => Some(text_string(bytes)),
            _ => None,
        }
    }

    /// The decoded data of `stream`, its filters applied in order as PDFBox applies them, each
    /// with its own decode parameters.
    pub(super) fn decode(&self, stream: &Stream) -> Result<Vec<u8>, Unported> {
        let filters: Vec<&[u8]> = match self.get(&stream.dict, b"Filter") {
            None => return Ok(stream.content.clone()),
            Some(Object::Name(name)) => vec![name.as_slice()],
            Some(Object::Array(names)) => names
                .iter()
                .filter_map(|name| match self.resolve(name) {
                    Some(Object::Name(name)) => Some(name.as_slice()),
                    _ => None,
                })
                .collect(),
            Some(_) => return Ok(stream.content.clone()),
        };
        let parameters = self.get(&stream.dict, b"DecodeParms");
        let mut data = stream.content.clone();
        for (index, filter) in filters.iter().enumerate() {
            let params = match parameters {
                Some(Object::Array(each)) => each
                    .get(index)
                    .and_then(|params| self.resolve(params))
                    .and_then(as_dict),
                Some(other) => (index == 0).then(|| as_dict(other)).flatten(),
                None => None,
            };
            data = decode_filter(filter, params, data)?;
        }
        Ok(data)
    }
}

/// One filter applied to `data`, by `lopdf`'s decoders.
fn decode_filter(
    filter: &[u8],
    params: Option<&Dictionary>,
    data: Vec<u8>,
) -> Result<Vec<u8>, Unported> {
    let name = match filter {
        b"FlateDecode" | b"Fl" => "FlateDecode",
        b"LZWDecode" | b"LZW" => "LZWDecode",
        b"ASCII85Decode" | b"A85" => "ASCII85Decode",
        b"ASCIIHexDecode" | b"AHx" => "ASCIIHexDecode",
        b"RunLengthDecode" | b"RL" => "RunLengthDecode",
        other => {
            return Err(Unported(format!(
                "a stream is encoded with the filter {} the port does not decode",
                name_string(other)
            )))
        }
    };
    let mut dict = Dictionary::new();
    dict.set("Filter", Object::Name(name.as_bytes().to_vec()));
    if let Some(params) = params {
        dict.set("DecodeParms", Object::Dictionary(params.clone()));
    }
    Stream::new(dict, data)
        .decompressed_content()
        .map_err(|e| Unported(format!("a stream's {name} data does not decode: {e}")))
}

/// The dictionary of a dictionary or a stream, which PDFBox's `COSStream` extends.
pub(super) fn as_dict(object: &Object) -> Option<&Dictionary> {
    match object {
        Object::Dictionary(dict) => Some(dict),
        Object::Stream(stream) => Some(&stream.dict),
        _ => None,
    }
}

/// `COSNumber.intValue`: an integer cast as Java casts a `long` to `int`, a real truncated.
pub(super) fn int_value(object: &Object) -> Option<i32> {
    match object {
        Object::Integer(value) => Some(*value as i32),
        Object::Real(value) => Some(*value as i32),
        _ => None,
    }
}

/// `COSNumber.floatValue`.
pub(super) fn float_value(object: &Object) -> Option<f32> {
    match object {
        Object::Integer(value) => Some(*value as f32),
        Object::Real(value) => Some(*value),
        _ => None,
    }
}

/// `COSString.getString`: UTF-16 after a byte order mark, otherwise PDFDocEncoding.
pub(super) fn text_string(bytes: &[u8]) -> String {
    match bytes {
        [0xFE, 0xFF, rest @ ..] => decode_java("UTF-16BE", rest),
        [0xFF, 0xFE, rest @ ..] => decode_java("UTF-16LE", rest),
        _ => bytes.iter().map(|&byte| pdf_doc_char(byte)).collect(),
    }
}

/// `new String(bytes, charset)` for a charset the JDK always has.
pub(super) fn decode_java(charset: &str, bytes: &[u8]) -> String {
    Charset::named(charset)
        .decode(bytes)
        .unwrap_or_else(|| panic!("the port decodes {charset}"))
}

/// `COSName.getName`: the bytes of a name as UTF-8 when they are valid UTF-8, else as
/// Windows-1252.
pub(super) fn name_string(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => decode_java("windows-1252", bytes),
    }
}

/// The character PDFDocEncoding gives a byte, as PDFBox's table holds it: Latin-1 with the
/// deviations of ISO 32000-1, and U+0000 for the bytes it leaves undefined.
pub(super) fn pdf_doc_char(byte: u8) -> char {
    match byte {
        0x18 => '\u{02D8}',
        0x19 => '\u{02C7}',
        0x1A => '\u{02C6}',
        0x1B => '\u{02D9}',
        0x1C => '\u{02DD}',
        0x1D => '\u{02DB}',
        0x1E => '\u{02DA}',
        0x1F => '\u{02DC}',
        0x7F => '\u{FFFD}',
        0x80 => '\u{2022}',
        0x81 => '\u{2020}',
        0x82 => '\u{2021}',
        0x83 => '\u{2026}',
        0x84 => '\u{2014}',
        0x85 => '\u{2013}',
        0x86 => '\u{0192}',
        0x87 => '\u{2044}',
        0x88 => '\u{2039}',
        0x89 => '\u{203A}',
        0x8A => '\u{2212}',
        0x8B => '\u{2030}',
        0x8C => '\u{201E}',
        0x8D => '\u{201C}',
        0x8E => '\u{201D}',
        0x8F => '\u{2018}',
        0x90 => '\u{2019}',
        0x91 => '\u{201A}',
        0x92 => '\u{2122}',
        0x93 => '\u{FB01}',
        0x94 => '\u{FB02}',
        0x95 => '\u{0141}',
        0x96 => '\u{0152}',
        0x97 => '\u{0160}',
        0x98 => '\u{0178}',
        0x99 => '\u{017D}',
        0x9A => '\u{0131}',
        0x9B => '\u{0142}',
        0x9C => '\u{0153}',
        0x9D => '\u{0161}',
        0x9E => '\u{017E}',
        0x9F => '\u{FFFD}',
        0xA0 => '\u{20AC}',
        0xAD => '\u{0}',
        _ => char::from(byte),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_text_strings_as_pdfbox_does() {
        assert_eq!(text_string(b"\xFE\xFF\x00A\x00\xE6"), "Aæ");
        assert_eq!(text_string(b"\xFF\xFEA\x00"), "A");
        assert_eq!(text_string(b"a\x80b\xAD"), "a\u{2022}b\u{0}");
        assert_eq!(text_string(b"\xFE\xFF\x00"), "\u{FFFD}");
    }

    #[test]
    fn names_are_utf8_or_windows_1252() {
        assert_eq!(name_string(b"F1"), "F1");
        assert_eq!(name_string("æ".as_bytes()), "æ");
        assert_eq!(name_string(b"\xE6"), "æ");
    }

    #[test]
    fn numbers_convert_as_java_casts_them() {
        assert_eq!(int_value(&Object::Real(2.9)), Some(2));
        assert_eq!(int_value(&Object::Real(-2.9)), Some(-2));
        assert_eq!(int_value(&Object::Integer(1 << 32)), Some(0));
        assert_eq!(float_value(&Object::Integer(3)), Some(3.0));
        assert_eq!(int_value(&Object::Null), None);
    }
}
