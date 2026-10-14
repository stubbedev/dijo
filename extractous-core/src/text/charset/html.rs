//! A port of Tika's `HtmlEncodingDetector`: the charset an HTML page's `<meta>` declares in its
//! first 8 KiB, read as ASCII, with comments ignored unless only a commented-out declaration is
//! found. Labels resolve as Tika resolves them: its WHATWG-style aliases first, then the JDK's
//! charset names, except labels IANA does not register.

use super::jdk::Charset;

const MARK_LIMIT: usize = 8192;

/// Labels the JDK knows that IANA does not register (`StandardCharsets_unsupported_by_IANA.txt`),
/// which a page declaring is not taken at its word.
const UNSUPPORTED_BY_IANA: &[&str] = &[
    "646",
    "737",
    "775",
    "813",
    "819",
    "858",
    "874",
    "8859_1",
    "8859_13",
    "8859_15",
    "8859_2",
    "8859_4",
    "8859_5",
    "8859_7",
    "8859_9",
    "912",
    "914",
    "915",
    "920",
    "923",
    "ansi-1251",
    "ascii",
    "ascii7",
    "cesu8",
    "cp1250",
    "cp1251",
    "cp1252",
    "cp1253",
    "cp1254",
    "cp1257",
    "cp5346",
    "cp5347",
    "cp5348",
    "cp5349",
    "cp5350",
    "cp5353",
    "cp737",
    "cp813",
    "cp858",
    "cp874",
    "cp912",
    "cp914",
    "cp915",
    "cp920",
    "cp923",
    "csibm862",
    "csisolatin0",
    "csisolatin9",
    "cspcp855",
    "default",
    "ibm-437",
    "ibm-737",
    "ibm737",
    "ibm-775",
    "ibm-813",
    "ibm813",
    "ibm-819",
    "ibm-850",
    "ibm-852",
    "ibm-855",
    "ibm-857",
    "ibm-862",
    "ibm-866",
    "ibm-874",
    "ibm874",
    "ibm-912",
    "ibm912",
    "ibm-914",
    "ibm914",
    "ibm-915",
    "ibm915",
    "ibm-920",
    "ibm920",
    "ibm-923",
    "ibm923",
    "iso_8859_1",
    "iso8859-1",
    "iso8859_1",
    "iso_8859-13",
    "iso8859-13",
    "iso8859_13",
    "iso8859-15",
    "iso8859_15",
    "iso8859_15_fdis",
    "iso8859-2",
    "iso8859_2",
    "iso8859-4",
    "iso8859_4",
    "iso8859-5",
    "iso8859_5",
    "iso8859-7",
    "iso8859_7",
    "iso8859-9",
    "iso8859_9",
    "koi8",
    "koi8_r",
    "koi8_u",
    "l9",
    "latin0",
    "latin9",
    "sun_eu_greek",
    "unicode",
    "unicode-1-1-utf-8",
    "unicodebig",
    "unicodebigunmarked",
    "unicodelittle",
    "unicodelittleunmarked",
    "utf_16",
    "utf16",
    "utf_16be",
    "utf_16le",
    "utf_32",
    "utf32",
    "utf_32be",
    "utf-32be-bom",
    "utf_32be_bom",
    "utf_32le",
    "utf-32le-bom",
    "utf_32le_bom",
    "utf8",
    "windows-437",
    "x-utf-16be",
    "x-utf-16le",
    "x-utf-32be",
    "x-utf-32le",
];

/// The charset a page's meta tags declare.
pub(super) fn detect(input: &[u8]) -> Option<Charset> {
    let head = &input[..input.len().min(MARK_LIMIT)];
    if !contains_meta_tag(head) {
        return None;
    }
    let text: String = head
        .iter()
        .map(|&b| if b < 0x80 { char::from(b) } else { '\u{FFFD}' })
        .collect();
    if text.contains("<!--") {
        find_charset(&without_comments(&text)).or_else(|| find_charset(&text))
    } else {
        find_charset(&text)
    }
}

fn contains_meta_tag(bytes: &[u8]) -> bool {
    bytes.iter().enumerate().any(|(i, &b)| {
        if b != b'<' {
            return false;
        }
        let rest = &bytes[i + 1..];
        let start = rest
            .iter()
            .position(|&c| !matches!(c, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r'))
            .unwrap_or(rest.len());
        rest.get(start..start + 4)
            .is_some_and(|name| name.iter().map(|&c| c | 0x20).eq(*b"meta"))
    })
}

/// The text with each comment replaced by a space, as Java's `<!--.*?(-->|$)` replaces them:
/// a comment ends at `-->` on its line, or at the end of the text on the last line; a comment
/// that runs on past a line break is left as it is.
fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        let body = &rest[start + 4..];
        let line_end = body.find(['\n', '\r']).unwrap_or(body.len());
        let end = match body[..line_end].find("-->") {
            Some(close) => Some(start + 4 + close + 3),
            None if is_final_line_break(&body[line_end..]) => Some(start + 4 + line_end),
            None => None,
        };
        match end {
            Some(end) => {
                out.push_str(&rest[..start]);
                out.push(' ');
                rest = &rest[end..];
            }
            None => {
                out.push_str(&rest[..start + 1]);
                rest = &rest[start + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Whether what is left is the text's end, or only a line break before it, where `$` matches.
fn is_final_line_break(rest: &str) -> bool {
    matches!(rest, "" | "\n" | "\r" | "\r\n")
}

/// The first charset a `charset=` attribute of a meta tag names that resolves.
fn find_charset(text: &str) -> Option<Charset> {
    let mut i = 0;
    while let Some(attributes) = next_meta_attributes(text, &mut i) {
        for label in charset_labels(attributes) {
            if UNSUPPORTED_BY_IANA.contains(&label.to_ascii_lowercase().as_str()) {
                continue;
            }
            if let Some(charset) = tika_alias(label).or_else(|| Charset::for_name(label)) {
                return Some(charset);
            }
        }
    }
    None
}

/// The attributes of the next meta tag at or after `*from`, as `(?is)<\s*meta(?:/|\s+)([^<>]+)`
/// finds them, moving `*from` past them.
fn next_meta_attributes<'a>(text: &'a str, from: &mut usize) -> Option<&'a str> {
    let bytes = text.as_bytes();
    while *from < bytes.len() {
        let start = *from;
        *from += 1;
        if bytes[start] != b'<' {
            continue;
        }
        let mut i = start + 1;
        while i < bytes.len() && is_java_space(bytes[i]) {
            i += 1;
        }
        if !bytes
            .get(i..i + 4)
            .is_some_and(|name| name.eq_ignore_ascii_case(b"meta"))
        {
            continue;
        }
        i += 4;
        match bytes.get(i) {
            Some(b'/') => i += 1,
            Some(&c) if is_java_space(c) => {
                while i < bytes.len() && is_java_space(bytes[i]) {
                    i += 1;
                }
            }
            _ => continue,
        }
        let end = bytes[i..]
            .iter()
            .position(|&c| c == b'<' || c == b'>')
            .map_or(bytes.len(), |offset| i + offset);
        if end == i {
            continue;
        }
        *from = end;
        return Some(&text[i..end]);
    }
    None
}

/// The labels of the `charset=` settings in a tag's attributes, as
/// `(?is)\bcharset\s*=\s*(?:['"]\s*)?([-_:\.a-z0-9]+)` finds them.
fn charset_labels(attributes: &str) -> Vec<&str> {
    let bytes = attributes.as_bytes();
    let mut labels = Vec::new();
    let mut i = 0;
    while i + 7 <= bytes.len() {
        let at_boundary = i == 0 || !is_word_byte(bytes[i - 1]);
        if !(at_boundary && bytes[i..i + 7].eq_ignore_ascii_case(b"charset")) {
            i += 1;
            continue;
        }
        let mut j = i + 7;
        while j < bytes.len() && is_java_space(bytes[j]) {
            j += 1;
        }
        if bytes.get(j) != Some(&b'=') {
            i += 1;
            continue;
        }
        j += 1;
        while j < bytes.len() && is_java_space(bytes[j]) {
            j += 1;
        }
        if matches!(bytes.get(j), Some(b'\'' | b'"')) {
            j += 1;
            while j < bytes.len() && is_java_space(bytes[j]) {
                j += 1;
            }
        }
        let start = j;
        while j < bytes.len() && is_label_byte(bytes[j]) {
            j += 1;
        }
        if j > start {
            labels.push(&attributes[start..j]);
            i = j;
        } else {
            i += 1;
        }
    }
    labels
}

fn is_java_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_label_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.')
}

/// Tika's own label table (`TikaHtmlCharsetAliases`), WHATWG's labels for the charsets pages
/// declare.
fn tika_alias(label: &str) -> Option<Charset> {
    let name = match label.trim().to_ascii_lowercase().as_str() {
        "big5" | "big5-hkscs" | "cn-big5" | "csbig5" | "x-x-big5" => "Big5",
        "cseucpkdfmtjapanese" | "euc-jp" | "x-euc-jp" => "EUC-JP",
        "cseuckr" | "csksc56011987" | "euc-kr" | "iso-ir-149" | "korean" | "ks_c_5601-1987"
        | "ks_c_5601-1989" | "ksc5601" | "ksc_5601" => "EUC-KR",
        "windows-949" | "ms949" | "cp949" => "x-windows-949",
        "chinese" | "csgb2312" | "csiso58gb231280" | "gb2312" | "gb_2312" | "gb_2312-80"
        | "gbk" | "iso-ir-58" | "x-gbk" => "GBK",
        "866" | "cp866" | "csibm866" | "ibm866" => "IBM866",
        "csiso2022jp" | "iso-2022-jp" => "ISO-2022-JP",
        "iso-8859-13" | "iso8859-13" | "iso885913" => "ISO-8859-13",
        "csisolatin9" | "iso-8859-15" | "iso8859-15" | "iso885915" | "iso_8859-15" | "l9" => {
            "ISO-8859-15"
        }
        "csisolatin2" | "iso-8859-2" | "iso-ir-101" | "iso8859-2" | "iso88592" | "iso_8859-2"
        | "iso_8859-2:1987" | "l2" | "latin2" => "ISO-8859-2",
        "csisolatin3" | "iso-8859-3" | "iso-ir-109" | "iso8859-3" | "iso88593" | "iso_8859-3"
        | "iso_8859-3:1988" | "l3" | "latin3" => "ISO-8859-3",
        "csisolatin4" | "iso-8859-4" | "iso-ir-110" | "iso8859-4" | "iso88594" | "iso_8859-4"
        | "iso_8859-4:1988" | "l4" | "latin4" => "ISO-8859-4",
        "csisolatincyrillic" | "cyrillic" | "iso-8859-5" | "iso-ir-144" | "iso8859-5"
        | "iso88595" | "iso_8859-5" | "iso_8859-5:1988" => "ISO-8859-5",
        "arabic" | "asmo-708" | "csiso88596e" | "csiso88596i" | "csisolatinarabic" | "ecma-114"
        | "iso-8859-6" | "iso-8859-6-e" | "iso-8859-6-i" | "iso-ir-127" | "iso8859-6"
        | "iso88596" | "iso_8859-6" | "iso_8859-6:1987" => "ISO-8859-6",
        "csisolatingreek" | "ecma-118" | "elot_928" | "greek" | "greek8" | "iso-8859-7"
        | "iso-ir-126" | "iso8859-7" | "iso88597" | "iso_8859-7" | "iso_8859-7:1987"
        | "sun_eu_greek" => "ISO-8859-7",
        "csiso88598e" | "csisolatinhebrew" | "hebrew" | "iso-8859-8" | "iso-8859-8-e"
        | "iso-ir-138" | "iso8859-8" | "iso88598" | "iso_8859-8" | "iso_8859-8:1988" | "visual"
        | "csiso88598i" | "iso-8859-8-i" | "logical" => "ISO-8859-8",
        "cskoi8r" | "koi" | "koi8" | "koi8-r" | "koi8_r" => "KOI8-R",
        "koi8-ru" | "koi8-u" => "KOI8-U",
        "csshiftjis" | "ms932" | "ms_kanji" | "shift-jis" | "shift_jis" | "sjis"
        | "windows-31j" | "x-sjis" => "Shift_JIS",
        "utf-16be" => "UTF-16BE",
        "utf-16" | "utf-16le" => "UTF-16LE",
        "unicode-1-1-utf-8" | "utf-8" | "utf8" => "UTF-8",
        "gb18030" => "GB18030",
        "cp1250" | "windows-1250" | "x-cp1250" => "windows-1250",
        "cp1251" | "windows-1251" | "x-cp1251" => "windows-1251",
        "ansi_x3.4-1968" | "ascii" | "cp1252" | "cp819" | "csisolatin1" | "ibm819"
        | "iso-8859-1" | "iso-ir-100" | "iso8859-1" | "iso88591" | "iso_8859-1"
        | "iso_8859-1:1987" | "l1" | "latin1" | "us-ascii" | "windows-1252" | "x-cp1252"
        | "x-user-defined" => "windows-1252",
        "cp1253" | "windows-1253" | "x-cp1253" => "windows-1253",
        "cp1254" | "csisolatin5" | "iso-8859-9" | "iso-ir-148" | "iso8859-9" | "iso88599"
        | "iso_8859-9" | "iso_8859-9:1989" | "l5" | "latin5" | "windows-1254" | "x-cp1254" => {
            "windows-1254"
        }
        "cp1255" | "windows-1255" | "x-cp1255" => "windows-1255",
        "cp1256" | "windows-1256" | "x-cp1256" => "windows-1256",
        "cp1257" | "windows-1257" | "x-cp1257" => "windows-1257",
        "cp1258" | "windows-1258" | "x-cp1258" => "windows-1258",
        "dos-874" | "iso-8859-11" | "iso8859-11" | "iso885911" | "tis-620" | "windows-874" => {
            "x-windows-874"
        }
        "x-mac-cyrillic" | "x-mac-ukrainian" => "x-MacCyrillic",
        "csmacintosh" | "mac" | "macintosh" | "x-mac-roman" => "x-MacRoman",
        _ => return None,
    };
    Some(Charset::named(name))
}
