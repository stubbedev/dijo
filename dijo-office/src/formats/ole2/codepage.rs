//! POI's `CodePageUtil`: the Java charset a Windows code page names, as
//! [`crate::text::codepage::encoding`] names it, and text decoded with it.

use super::property_set::Fault;
use crate::formats::poi::{Thrown, Unread};
use crate::formats::Unported;
use crate::text::charset::Charset;
use crate::text::codepage::encoding;

/// The code page of UTF-16LE.
pub(super) const UNICODE: i32 = 1200;
/// The code page POI reads text in where a section names none.
pub(super) const DEFAULT: i32 = 1252;

/// The JDK charset of a code page: `Fault::Encoding` where POI refuses the code page or the JDK
/// has no charset of its name, as `new String(bytes, name)` throws.
pub(super) fn charset(codepage: i32) -> Result<Charset, Fault> {
    encoding(codepage)
        .and_then(|name| Charset::for_name(&name))
        .ok_or(Fault::Encoding)
}

/// `CodePageUtil.getStringFromCodePage`.
pub(super) fn decode(bytes: &[u8], codepage: i32) -> Result<String, Fault> {
    let charset = charset(codepage)?;
    charset.decode(bytes).ok_or_else(|| {
        Fault::Unported(format!(
            "text in code page {codepage}, which the native decoders do not decode"
        ))
    })
}

/// `CodePageUtil.getStringFromCodePage` for a reader outside the property sets: the text, or
/// why it is not read: POI throws on the code page, or the native decoders do not decode it.
pub(crate) fn decode_text(bytes: &[u8], codepage: i32) -> Result<String, Unread> {
    decode(bytes, codepage).map_err(|fault| match fault {
        Fault::Unported(reason) => Unported(reason).into(),
        _ => Thrown::new(format!(
            "java.io.UnsupportedEncodingException: code page {codepage}"
        ))
        .into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_charsets_as_poi_does() {
        assert_eq!(charset(850).unwrap().name(), "IBM850");
        assert_eq!(charset(10000).unwrap().name(), "x-MacRoman");
        assert_eq!(charset(10008).unwrap().name(), "GB2312");
        assert!(matches!(charset(4), Err(Fault::Encoding)));
    }

    #[test]
    fn decodes_as_java_does() {
        assert_eq!(decode(b"caf\xe9", 1252).unwrap(), "café");
        assert_eq!(decode(b"a\0b\0", UNICODE).unwrap(), "ab");
        assert!(matches!(decode(b"x", -1), Err(Fault::Encoding)));
    }
}
