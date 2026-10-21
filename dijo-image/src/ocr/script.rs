//! Our Java `ScriptDetection`'s choice of a language model for a script among the models that
//! are installed, the model of each national language of Europe the language detector tells
//! apart, and `PreparingOcrParser`'s test of whether a reading is written in a script.

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

/// The model of each national language of Europe the language detector tells apart, by the
/// detector's ISO 639-3 code and the script of the text. Northern Sami has no tesseract model and
/// reads with its script's.
const MODEL_BY_LANGUAGE: [(&str, &str, &str); 44] = [
    ("bel", "Cyrillic", "bel"),
    ("bos", "Latin", "bos"),
    ("bul", "Cyrillic", "bul"),
    ("cat", "Latin", "cat"),
    ("ces", "Latin", "ces"),
    ("cym", "Latin", "cym"),
    ("dan", "Latin", "dan"),
    ("deu", "Latin", "deu"),
    ("ell", "Greek", "ell"),
    ("eng", "Latin", "eng"),
    ("est", "Latin", "est"),
    ("eus", "Latin", "eus"),
    ("fao", "Latin", "fao"),
    ("fin", "Latin", "fin"),
    ("fra", "Latin", "fra"),
    ("gle", "Latin", "gle"),
    ("glg", "Latin", "glg"),
    ("hrv", "Latin", "hrv"),
    ("hun", "Latin", "hun"),
    ("hye", "Armenian", "hye"),
    ("isl", "Latin", "isl"),
    ("ita", "Latin", "ita"),
    ("kat", "Georgian", "kat"),
    ("lav", "Latin", "lav"),
    ("lit", "Latin", "lit"),
    ("ltz", "Latin", "ltz"),
    ("mkd", "Cyrillic", "mkd"),
    ("mlt", "Latin", "mlt"),
    ("nld", "Latin", "nld"),
    ("nno", "Latin", "nor"),
    ("nob", "Latin", "nor"),
    ("pol", "Latin", "pol"),
    ("por", "Latin", "por"),
    ("ron", "Latin", "ron"),
    ("rus", "Cyrillic", "rus"),
    ("slk", "Latin", "slk"),
    ("slv", "Latin", "slv"),
    ("spa", "Latin", "spa"),
    ("sqi", "Latin", "sqi"),
    ("srp", "Cyrillic", "srp"),
    ("srp", "Latin", "srp_latn"),
    ("swe", "Latin", "swe"),
    ("tur", "Latin", "tur"),
    ("ukr", "Cyrillic", "ukr"),
];

/// The installed model of a language, by the detector's ISO 639-3 code, in a script, or none
/// when it has none or none is installed.
pub(crate) fn language_model<'i>(
    installed: &'i BTreeSet<String>,
    language: &str,
    script: &str,
) -> Option<&'i str> {
    let (_, _, model) = MODEL_BY_LANGUAGE
        .iter()
        .find(|(code, written_in, _)| *code == language && *written_in == script)?;
    installed.get(*model).map(String::as_str)
}

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

    #[test]
    fn picks_the_model_of_a_language_in_its_script() {
        let models = installed(&["nor", "spa", "srp", "srp_latn"]);
        assert_eq!(language_model(&models, "spa", "Latin"), Some("spa"));
        assert_eq!(language_model(&models, "nno", "Latin"), Some("nor"));
        assert_eq!(language_model(&models, "srp", "Cyrillic"), Some("srp"));
        assert_eq!(language_model(&models, "srp", "Latin"), Some("srp_latn"));
        assert_eq!(language_model(&models, "spa", "Cyrillic"), None);
        assert_eq!(language_model(&models, "fra", "Latin"), None);
        assert_eq!(language_model(&models, "sme", "Latin"), None);
    }

    /// The OCR bundle and the models the dev shell and CI read with are those of `ci/tessdata.tsv`.
    fn bundled() -> BTreeSet<String> {
        std::fs::read_to_string("../ci/tessdata.tsv")
            .unwrap()
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| line.split('\t').next().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn every_national_language_of_europe_has_a_bundled_model() {
        let bundled = bundled();
        let tatoeba =
            std::fs::read_to_string("../dijo-base/tests/fixtures/language/tatoeba.tsv").unwrap();
        let labels: BTreeSet<&str> = tatoeba
            .lines()
            .filter_map(|line| line.split('\t').next())
            .collect();
        let missing: Vec<&str> = labels
            .into_iter()
            .filter(|label| *label != "sme")
            .filter(|label| {
                let (code, script) = match label.split_once('-') {
                    Some((code, "Latn")) => (code, "Latin"),
                    Some((_, other)) => panic!("unknown script suffix {other}"),
                    None => (*label, default_script(label)),
                };
                language_model(&bundled, code, script).is_none()
            })
            .collect();
        assert!(missing.is_empty(), "no bundled model for {missing:?}");
        for (_, _, model) in MODEL_BY_LANGUAGE {
            assert!(bundled.contains(model), "{model} is not bundled");
        }
        for (_, model) in LANGUAGE_BY_SCRIPT {
            assert!(bundled.contains(model), "{model} is not bundled");
        }
    }

    /// The script a language of the fixture without a script suffix is written in.
    fn default_script(code: &str) -> &'static str {
        MODEL_BY_LANGUAGE
            .iter()
            .find(|(language, _, _)| *language == code)
            .map_or("Latin", |(_, script, _)| script)
    }
}
