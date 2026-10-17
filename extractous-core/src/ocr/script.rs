//! Our Java `ScriptDetection`'s choice of a language model for a script among the models that
//! are installed, and `PreparingOcrParser`'s test of whether a reading is written in a script.

use std::collections::BTreeSet;

use unicode_script::Script;

use crate::unicode::{is_letter, script};

/// The model read with when a script has none installed.
const DEFAULT_LANGUAGE: &str = "eng";
const SCRIPT_MODEL_PREFIX: &str = "script/";

/// The most common language of each script tesseract's detection reports.
const LANGUAGE_BY_SCRIPT: [(&str, &str); 25] = [
    ("Latin", "eng"),
    ("Arabic", "ara"),
    ("Cyrillic", "rus"),
    ("Greek", "ell"),
    ("Hebrew", "heb"),
    ("Han", "chi_sim"),
    ("Japanese", "jpn"),
    ("Hangul", "kor"),
    ("Thai", "tha"),
    ("Devanagari", "hin"),
    ("Bengali", "ben"),
    ("Tamil", "tam"),
    ("Telugu", "tel"),
    ("Kannada", "kan"),
    ("Malayalam", "mal"),
    ("Gujarati", "guj"),
    ("Gurmukhi", "pan"),
    ("Georgian", "kat"),
    ("Armenian", "script/Armenian"),
    ("Ethiopic", "amh"),
    ("Khmer", "khm"),
    ("Lao", "lao"),
    ("Myanmar", "mya"),
    ("Sinhala", "sin"),
    ("Tibetan", "bod"),
];

/// The model to read a script with: the script model, then the script's most common language,
/// if installed, and English otherwise.
pub(crate) fn language(installed: &BTreeSet<String>, script: Option<&str>) -> String {
    script
        .and_then(|script| installed_language(installed, script))
        .unwrap_or_else(|| DEFAULT_LANGUAGE.to_owned())
}

/// The installed model for a script: the script model, then the script's most common language,
/// or none when neither is installed.
pub(crate) fn installed_language(installed: &BTreeSet<String>, script: &str) -> Option<String> {
    let script_model = format!("{SCRIPT_MODEL_PREFIX}{script}");
    if installed.contains(&script_model) {
        return Some(script_model);
    }
    LANGUAGE_BY_SCRIPT
        .iter()
        .find(|(name, _)| *name == script)
        .map(|(_, language)| (*language).to_owned())
        .filter(|language| installed.contains(language))
}

/// Whether most letters of a text are in a script, named as tesseract names it. A model for a
/// script that detection overlooked reads a page in another script with the Latin letters its
/// model also has, often with more confidence than a reading spoiled by noise, and without that
/// page's diacritics. A script Unicode does not name, such as tesseract's `Japanese`, holds no
/// letters.
pub(crate) fn written_in(text: &str, script_name: &str) -> bool {
    let Some(expected) = Script::from_full_name(script_name) else {
        return false;
    };
    let (letters, in_script) = text
        .chars()
        .filter(|&c| is_letter(c))
        .fold((0u64, 0u64), |(letters, in_script), c| {
            (letters + 1, in_script + u64::from(script(c) == expected))
        });
    in_script * 2 > letters
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(models: &[&str]) -> BTreeSet<String> {
        models.iter().map(|model| (*model).to_owned()).collect()
    }

    #[test]
    fn picks_the_script_model_then_the_scripts_language() {
        let models = installed(&["eng", "rus", "script/Latin", "script/Armenian"]);
        assert_eq!(language(&models, Some("Latin")), "script/Latin");
        assert_eq!(language(&models, Some("Cyrillic")), "rus");
        assert_eq!(language(&models, Some("Greek")), "eng");
        assert_eq!(language(&models, None), "eng");
        assert_eq!(installed_language(&models, "Greek"), None);
        assert_eq!(
            installed_language(&models, "Armenian").as_deref(),
            Some("script/Armenian")
        );
    }

    #[test]
    fn tells_the_script_most_letters_are_in() {
        assert!(written_in("Բարեւ աշխարհ, hi", "Armenian"));
        assert!(!written_in("hello world, Բարեւ", "Armenian"));
        assert!(!written_in("日本語", "Japanese"));
    }
}
