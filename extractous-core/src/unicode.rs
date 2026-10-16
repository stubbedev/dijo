//! Java's `Character` classification of code points, for ports of Tika code whose results must
//! match Java's. The Unicode data comes from Rust's standard library and the `unicode-*` crates,
//! which may be a Unicode version ahead of the JDK Tika runs on; they differ only on characters
//! that version added.

use unicode_general_category::{get_general_category, GeneralCategory};
use unicode_script::{Script, UnicodeScript};

/// `Character.isLetter`: the general categories L*.
pub fn is_letter(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
    )
}

/// `Character.getType` of a cased letter: the general categories Lu, Ll and Lt.
pub fn is_cased_letter(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
    )
}

/// `Character.getType` of a combining mark: the general categories Mn, Mc and Me.
pub fn is_mark(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::NonspacingMark
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
    )
}

/// `Character.isUpperCase`: the Uppercase property.
pub fn is_uppercase(c: char) -> bool {
    c.is_uppercase()
}

/// `Character.isLowerCase`: the Lowercase property.
pub fn is_lowercase(c: char) -> bool {
    c.is_lowercase()
}

/// `Character.toLowerCase(int)`, the simple lowercase mapping: the first character of the full
/// mapping, which only differs for İ, whose full mapping adds a combining dot.
pub fn lowercase(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// `Character.isSpaceChar`: the general categories Zs, Zl and Zp.
pub fn is_space_char(c: char) -> bool {
    matches!(
        get_general_category(c),
        GeneralCategory::SpaceSeparator
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
    )
}

/// `Character.isWhitespace`: a separator other than the no-break spaces, or one of the ASCII
/// controls tab to carriage return and the file, group, record and unit separators.
pub fn is_whitespace(c: char) -> bool {
    match c {
        '\u{9}'..='\u{D}' | '\u{1C}'..='\u{20}' => true,
        '\u{0}'..='\u{7F}' | '\u{A0}' | '\u{2007}' | '\u{202F}' => false,
        _ => is_space_char(c),
    }
}

/// `Character.isISOControl`: U+0000 to U+001F and U+007F to U+009F.
pub fn is_iso_control(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1F}' | '\u{7F}'..='\u{9F}')
}

/// `Character.digit(c, 10)`: the value of a decimal digit of any script, the general category
/// Nd. Unicode encodes such digits in runs of ten from zero, so a digit's value is its distance
/// from the run's start, modulo ten for runs that follow each other directly.
pub fn decimal_digit(c: char) -> Option<u32> {
    if c.is_ascii_digit() {
        return Some(u32::from(c) - u32::from('0'));
    }
    let is_digit = |c: char| get_general_category(c) == GeneralCategory::DecimalNumber;
    if !is_digit(c) {
        return None;
    }
    let preceding = (1..=u32::from(c))
        .map_while(|back| char::from_u32(u32::from(c) - back).filter(|&d| is_digit(d)))
        .count();
    Some((preceding % 10) as u32)
}

/// `Character.UnicodeScript.of`: the Script property, without script extensions.
pub fn script(c: char) -> Script {
    if c.is_ascii() {
        return if c.is_ascii_alphabetic() {
            Script::Latin
        } else {
            Script::Common
        };
    }
    c.script()
}

/// Whether `Character.UnicodeScript.of` answers one of the pseudo-scripts COMMON, INHERITED or
/// UNKNOWN rather than a writing system.
pub fn is_pseudo_script(script: Script) -> bool {
    matches!(script, Script::Common | Script::Inherited | Script::Unknown)
}

/// The name of a script's `Character.UnicodeScript` constant, such as `LATIN` or
/// `CANADIAN_ABORIGINAL`.
pub fn java_script_name(script: Script) -> String {
    script.full_name().to_ascii_uppercase()
}

/// The Unicode names of scripts that are not their Java constant's words capitalized.
const IRREGULAR_SCRIPT_NAMES: [&str; 1] = ["SignWriting"];

/// The script whose `Character.UnicodeScript` constant is named `name`: the inverse of
/// [`java_script_name`], or `None` for a name that is no script. The Unicode name is the
/// constant's words capitalized, but for a few irregular names.
pub fn java_script(name: &str) -> Option<Script> {
    let full_name = match IRREGULAR_SCRIPT_NAMES
        .iter()
        .find(|irregular| irregular.eq_ignore_ascii_case(name))
    {
        Some(irregular) => (*irregular).to_owned(),
        None => {
            let mut capitalized = String::with_capacity(name.len());
            let mut word_start = true;
            for c in name.chars() {
                capitalized.push(if word_start {
                    c
                } else {
                    c.to_ascii_lowercase()
                });
                word_start = c == '_';
            }
            capitalized
        }
    };
    Script::from_full_name(&full_name).filter(|&script| java_script_name(script) == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_script_of_every_code_point_by_its_java_name() {
        let mut scripts: Vec<Script> = (0..=0x10_FFFFu32)
            .filter_map(char::from_u32)
            .map(script)
            .collect();
        scripts.sort_unstable_by_key(|script| script.full_name());
        scripts.dedup();
        assert!(scripts.len() > 150, "{} scripts", scripts.len());
        for script in scripts {
            assert_eq!(java_script(&java_script_name(script)), Some(script));
        }
        assert_eq!(java_script("SIGNWRITING"), Some(Script::SignWriting));
        assert_eq!(java_script("Latin"), None);
        assert_eq!(java_script("OTHER"), None);
    }

    #[test]
    fn reads_the_script_of_ascii_as_the_script_table_does() {
        for c in '\0'..='\u{7f}' {
            assert_eq!(script(c), c.script(), "{c:?}");
        }
    }
}
