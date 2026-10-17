//! The code pages an RTF document names, as `TextExtractor` maps them to Java charsets: the
//! `\ansicpgN` and `\fcharsetN` numbers and the `\ansi`, `\pc`, `\pca` and `\mac` words. A name
//! the JDK has no charset for stands for US-ASCII, as `TextExtractor.getCharset` falls back.

use crate::text::charset::Charset;

/// The charset `TextExtractor.getCharset(name)` finds.
pub(super) fn named(name: &str) -> Charset {
    Charset::for_name(name).unwrap_or_else(|| Charset::named("US-ASCII"))
}

pub(super) fn windows_1252() -> Charset {
    named("WINDOWS-1252")
}

pub(super) fn cp850() -> Charset {
    named("cp850")
}

pub(super) fn cp437() -> Charset {
    named("cp437")
}

pub(super) fn mac_roman() -> Charset {
    named("MacRoman")
}

/// The charset of `\fcharsetN`, if `TextExtractor` knows `N`.
pub(super) fn for_fcharset(number: i32) -> Option<Charset> {
    fcharset_name(number).map(named)
}

/// The charset of `\ansicpgN`, if `TextExtractor` knows `N`.
pub(super) fn for_ansicpg(number: i32) -> Option<Charset> {
    ansicpg_name(number).map(named)
}

fn fcharset_name(number: i32) -> Option<&'static str> {
    Some(match number {
        0 => "WINDOWS-1252",
        77 => "MacRoman",
        78 => "Shift_JIS",
        79 => "ms949",
        80 => "GB2312",
        81 => "Big5",
        82 => "johab",
        83 => "MacHebrew",
        84 => "MacArabic",
        85 => "MacGreek",
        86 => "MacTurkish",
        87 => "MacThai",
        88 => "cp1250",
        89 => "cp1251",
        128 => "MS932",
        129 => "ms949",
        130 => "ms1361",
        134 => "ms936",
        136 => "ms950",
        161 => "cp1253",
        162 => "cp1254",
        163 => "cp1258",
        177 => "cp1255",
        178 => "cp1256",
        186 => "cp1257",
        204 => "cp1251",
        222 => "ms874",
        238 => "cp1250",
        254 => "cp437",
        255 => "cp850",
        _ => return None,
    })
}

fn ansicpg_name(number: i32) -> Option<&'static str> {
    Some(match number {
        437 => "CP437",
        708 => "ISO-8859-6",
        709 => "windows-709",
        710 => "windows-720",
        819 => "CP819",
        850 => "CP850",
        852 => "CP852",
        860 => "CP860",
        862 => "CP862",
        863 => "CP863",
        864 => "CP864",
        865 => "CP865",
        866 => "CP866",
        874 => "MS874",
        932 => "MS932",
        936 => "MS936",
        949 => "CP949",
        950 => "CP950",
        1250 => "CP1250",
        1251 => "CP1251",
        1252 => "CP1252",
        1253 => "CP1253",
        1254 => "CP1254",
        1255 => "CP1255",
        1256 => "CP1256",
        1257 => "CP1257",
        1258 => "CP1258",
        1361 => "x-Johab",
        10000 => "MacRoman",
        10001 => "Shift_JIS",
        10004 => "MacArabic",
        10005 => "MacHebrew",
        10006 => "MacGreek",
        10007 => "MacCyrillic",
        10029 => "x-MacCentralEurope",
        10081 => "MacTurkish",
        57002 => "x-ISCII91",
        57003 => "windows-57003",
        57004 => "windows-57004",
        57005 => "windows-57005",
        57006 => "windows-57006",
        57007 => "windows-57007",
        57008 => "windows-57008",
        57009 => "windows-57009",
        57010 => "windows-57010",
        57011 => "windows-57011",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFERENCE: &str =
        include_str!("../../../../extractous-base/tests/fixtures/rtf/charsets.tsv");

    fn all_names() -> impl Iterator<Item = &'static str> {
        (0..=60_000)
            .filter_map(fcharset_name)
            .chain((0..=60_000).filter_map(ansicpg_name))
            .chain(["WINDOWS-1252", "cp850", "cp437", "MacRoman"])
    }

    #[test]
    fn resolves_names_as_tikas_charset_utils_does() {
        let reference: Vec<(&str, &str)> = REFERENCE
            .lines()
            .map(|line| line.split_once('\t').unwrap())
            .collect();
        for name in all_names() {
            let (_, expected) = reference
                .iter()
                .find(|(listed, _)| *listed == name)
                .unwrap_or_else(|| panic!("charsets.tsv lacks {name}"));
            assert_eq!(named(name).name(), *expected, "{name}");
        }
    }

    #[test]
    fn decodes_every_charset_it_names_but_iscii() {
        let undecodable: Vec<&str> = all_names()
            .filter(|name| named(name).decode(b"a\x80\xff").is_none())
            .collect();
        assert_eq!(undecodable, ["x-ISCII91"]);
        assert_eq!(ansicpg_name(57002), Some("x-ISCII91"));
    }
}
