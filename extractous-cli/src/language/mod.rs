use serde::Serialize;

mod charsoup;

use charsoup::Confidence;

/// How much text, from the start, the language is detected from.
const SAMPLE_BYTES: usize = 64 * 1024;

/// The language a text is written in, as detected from the text itself.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DetectedLanguage {
    /// ISO 639-3 code.
    pub code: &'static str,
    /// The Unicode script most of the letters are written in, such as `Latin` or `Han`.
    pub script: &'static str,
    pub confidence: f64,
    /// Whether the detection is confident enough to act on, such as to pick a search analyzer.
    pub reliable: bool,
}

/// The language of a text, or None when it has no letters or no language leads. A label with a
/// private-use orthography tag, such as `be-x-old` for Belarusian in the Taraškievica spelling,
/// is reported as its base language.
pub fn detect(text: &str) -> Option<DetectedLanguage> {
    charsoup::detect(text).map(|detection| DetectedLanguage {
        code: detection
            .label
            .split_once("-x-")
            .map_or(detection.label, |(base, _)| base),
        script: detection.script.full_name(),
        confidence: f64::from(detection.score),
        reliable: matches!(detection.confidence, Confidence::High | Confidence::Medium),
    })
}

/// The start of a text that arrives in chunks, kept to detect its language from.
#[derive(Default)]
pub struct Sample {
    bytes: Vec<u8>,
}

impl Sample {
    pub fn push(&mut self, bytes: &[u8]) {
        let room = SAMPLE_BYTES.saturating_sub(self.bytes.len());
        self.bytes
            .extend_from_slice(&bytes[..bytes.len().min(room)]);
    }

    pub fn detect(&self) -> Option<DetectedLanguage> {
        detect(&String::from_utf8_lossy(&self.bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_and_scripts_are_told_apart() {
        let english = detect("Every evening at dusk the keeper climbed the steps of the tower and wrote a single line in the ledger.").unwrap();
        assert_eq!((english.code, english.script), ("eng", "Latin"));
        assert!(english.reliable);
        let german = detect("Jeden Abend in der Dämmerung stieg der Wärter die Stufen des Turms hinauf und schrieb eine Zeile in das Logbuch.").unwrap();
        assert_eq!(german.code, "deu");
        let arabic = detect("كان الحارس يصعد درج البرج كل مساء عند الغروب، ويشعل المصباح الكبير، ثم يكتب سطراً واحداً في الدفتر.").unwrap();
        assert_eq!((arabic.code, arabic.script), ("ara", "Arabic"));
        assert_eq!(detect(""), None);
    }

    /// The least share of Tatoeba sentences, alone and in runs of five, detected as their
    /// language: Danish, its neighbours and the other Nordic languages first.
    const LEAST_ACCURACY: [(&str, f64, f64); 17] = [
        ("dan", 0.675, 0.975),
        ("nob", 0.685, 0.975),
        ("nno", 0.805, 1.0),
        ("swe", 0.71, 1.0),
        ("isl", 0.94, 1.0),
        ("fao", 0.85, 1.0),
        ("fin", 0.915, 1.0),
        ("sme", 0.814, 1.0),
        ("deu", 0.855, 1.0),
        ("nld", 0.73, 1.0),
        ("eng", 0.8, 1.0),
        ("fra", 0.845, 1.0),
        ("spa", 0.77, 1.0),
        ("ita", 0.81, 1.0),
        ("pol", 0.925, 1.0),
        ("ces", 0.855, 1.0),
        ("est", 0.795, 1.0),
    ];
    const RUN: usize = 5;

    #[test]
    fn nordic_and_neighbouring_languages_are_told_apart() {
        let tatoeba = include_str!("../../tests/fixtures/language/tatoeba.tsv");
        let mut sentences: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in tatoeba.lines() {
            let mut fields = line.splitn(3, '\t');
            let (Some(language), Some(_), Some(sentence)) =
                (fields.next(), fields.next(), fields.next())
            else {
                panic!("malformed fixture line {line:?}");
            };
            match sentences.iter_mut().find(|(known, _)| *known == language) {
                Some((_, list)) => list.push(sentence),
                None => sentences.push((language, vec![sentence])),
            }
        }
        let share = |texts: &[String], language: &str| {
            let right = texts
                .iter()
                .filter(|text| detect(text).is_some_and(|found| found.code == language))
                .count();
            right as f64 / texts.len() as f64
        };
        let mut failures = Vec::new();
        for (language, least_alone, least_in_runs) in LEAST_ACCURACY {
            let (_, list) = sentences
                .iter()
                .find(|(known, _)| *known == language)
                .unwrap_or_else(|| panic!("no {language} sentences in the fixture"));
            let alone: Vec<String> = list.iter().map(|s| s.to_string()).collect();
            let runs: Vec<String> = list
                .as_chunks::<RUN>()
                .0
                .iter()
                .map(|run| run.join(" "))
                .collect();
            let (alone, in_runs) = (share(&alone, language), share(&runs, language));
            if alone < least_alone || in_runs < least_in_runs {
                failures.push(format!(
                    "{language}: {alone:.3} of sentences (least {least_alone}), \
                     {in_runs:.3} of runs of {RUN} (least {least_in_runs})"
                ));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn the_sample_keeps_only_the_start() {
        let mut sample = Sample::default();
        sample.push(&vec![b'a'; SAMPLE_BYTES - 1]);
        sample.push(b"bc");
        assert_eq!(sample.bytes.len(), SAMPLE_BYTES);
        assert_eq!(*sample.bytes.last().unwrap(), b'b');
    }
}
