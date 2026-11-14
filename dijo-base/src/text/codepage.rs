//! Windows code pages and character sets, and the JDK charsets that decode them: as Windows
//! itself decodes a code page, as POI's `CodePageUtil` names the charset of one, and as POI's
//! `FontCharset` names the charset of a font's character set.

use std::borrow::Cow;

use super::charset::Charset;

/// The JDK charset Windows decodes the code page `code_page` with, if the JDK has one: the
/// charset of the alias `windows-N`, or `x-Johab` for code page 1361, which has no such alias.
pub fn windows(code_page: u16) -> Option<Charset> {
    match code_page {
        1361 => Charset::for_name("x-Johab"),
        other => Charset::for_name(&format!("windows-{other}")),
    }
}

/// POI's `CodePageUtil.codepageToEncoding(codepage, false)`: the name of the Java charset of a
/// code page, `None` for a code page below one, which POI refuses.
pub fn encoding(codepage: i32) -> Option<Cow<'static, str>> {
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

/// POI's `FontCharset.valueOf(id).getCharset()`: the name of the Java charset of the Windows
/// character set `id` a font names, `None` for the symbol set and for an id POI does not know.
pub fn font_charset(id: u8) -> Option<&'static str> {
    Some(match id {
        0x00 | 0x01 | 0xFF => "Cp1252",
        0x4D => "MacRoman",
        0x80 => "Shift_JIS",
        0x81 => "cp949",
        0x82 => "x-Johab",
        0x86 => "GB2312",
        0x88 => "Big5",
        0xA1 => "Cp1253",
        0xA2 => "Cp1254",
        0xA3 => "Cp1258",
        0xB1 => "Cp1255",
        0xB2 => "Cp1256",
        0xBA => "Cp1257",
        0xCC => "Cp1251",
        0xDE => "x-windows-874",
        0xEE => "Cp1250",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_code_pages_as_windows_does() {
        for (code_page, name) in [
            (874, "x-windows-874"),
            (932, "windows-31j"),
            (936, "GBK"),
            (949, "x-windows-949"),
            (950, "x-windows-950"),
            (1250, "windows-1250"),
            (1252, "windows-1252"),
            (1258, "windows-1258"),
            (1361, "x-Johab"),
        ] {
            let charset = windows(code_page).unwrap();
            assert_eq!(charset.name(), name, "{code_page}");
            assert!(charset.decodes(), "{code_page}");
        }
        assert_eq!(windows(9999), None);
    }

    #[test]
    fn names_charsets_as_poi_does() {
        assert_eq!(encoding(0), None);
        assert_eq!(encoding(-1), None);
        assert_eq!(encoding(1252).as_deref(), Some("windows-1252"));
        assert_eq!(encoding(932).as_deref(), Some("SJIS"));
        assert_eq!(encoding(850).as_deref(), Some("cp850"));
    }

    #[test]
    fn names_font_charsets_as_poi_does() {
        assert_eq!(font_charset(0x02), None);
        assert_eq!(font_charset(0x03), None);
        for id in 0..=u8::MAX {
            if let Some(name) = font_charset(id) {
                assert!(
                    Charset::for_name(name).is_some_and(Charset::decodes),
                    "{id:#04x} {name}"
                );
            }
        }
    }
}
