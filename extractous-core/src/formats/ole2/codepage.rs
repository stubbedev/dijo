//! POI's `CodePageUtil`: the Java charset a Windows code page names, and text decoded with it.

use std::borrow::Cow;

use super::property_set::Fault;
use crate::text::charset::Charset;

/// The code page of UTF-16LE.
pub(super) const UNICODE: i32 = 1200;
/// The code page POI reads text in where a section names none.
pub(super) const DEFAULT: i32 = 1252;

/// `CodePageUtil.codepageToEncoding(codepage, false)`: `None` for a code page below one, which
/// POI refuses.
pub(super) fn encoding(codepage: i32) -> Option<Cow<'static, str>> {
    let name = match codepage {
        ..=0 => return None,
        1200 => "UTF-16LE",
        1201 => "UTF-16BE",
        65001 => "UTF-8",
        37 => "cp037",
        936 => "GBK",
        949 => "ms949",
        1250 => "windows-1250",
        1251 => "windows-1251",
        1252 | 32769 => "windows-1252",
        1253 => "windows-1253",
        1254 => "windows-1254",
        1255 => "windows-1255",
        1256 => "windows-1256",
        1257 => "windows-1257",
        1258 => "windows-1258",
        1361 => "johab",
        10000 | 32768 => "MacRoman",
        10001 | 932 => "SJIS",
        10002 => "Big5",
        10003 | 51949 => "EUC-KR",
        10004 => "MacArabic",
        10005 => "MacHebrew",
        10006 => "MacGreek",
        10007 => "MacCyrillic",
        10008 => "EUC_CN",
        10010 => "MacRomania",
        10017 => "MacUkraine",
        10021 => "MacThai",
        10029 => "MacCentralEurope",
        10079 => "MacIceland",
        10081 => "MacTurkish",
        10082 => "MacCroatian",
        20127 | 65000 => "US-ASCII",
        20866 => "KOI8-R",
        28591 => "ISO-8859-1",
        28592 => "ISO-8859-2",
        28593 => "ISO-8859-3",
        28594 => "ISO-8859-4",
        28595 => "ISO-8859-5",
        28596 => "ISO-8859-6",
        28597 => "ISO-8859-7",
        28598 => "ISO-8859-8",
        28599 => "ISO-8859-9",
        50220..=50222 => "ISO-2022-JP",
        50225 => "ISO-2022-KR",
        51932 => "EUC-JP",
        52936 => "GB2312",
        54936 => "GB18030",
        other => return Some(Cow::Owned(format!("cp{other}"))),
    };
    Some(Cow::Borrowed(name))
}

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
    charset.decode_exactly(bytes).ok_or_else(|| {
        Fault::Unported(format!(
            "text in code page {codepage}, which this port does not decode as Java does"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_charsets_as_poi_does() {
        assert_eq!(encoding(0), None);
        assert_eq!(encoding(1252).as_deref(), Some("windows-1252"));
        assert_eq!(encoding(850).as_deref(), Some("cp850"));
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
