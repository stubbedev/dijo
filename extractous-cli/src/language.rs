use serde::Serialize;

/// How much text, from the start, the language is detected from.
const SAMPLE_BYTES: usize = 64 * 1024;

/// The language a text is written in, as detected from the text itself.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DetectedLanguage {
    /// ISO 639-3 code.
    pub code: &'static str,
    pub script: String,
    pub confidence: f64,
    /// Whether the detection is confident enough to act on, such as to pick a search analyzer.
    pub reliable: bool,
}

pub fn detect(text: &str) -> Option<DetectedLanguage> {
    whatlang::detect(text).map(|info| DetectedLanguage {
        code: info.lang().code(),
        script: info.script().name().to_string(),
        confidence: info.confidence(),
        reliable: info.is_reliable(),
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
        assert_eq!((english.code, english.script.as_str()), ("eng", "Latin"));
        assert!(english.reliable);
        let german = detect("Jeden Abend in der Dämmerung stieg der Wärter die Stufen des Turms hinauf und schrieb eine Zeile in das Logbuch.").unwrap();
        assert_eq!(german.code, "deu");
        let arabic = detect("كان الحارس يصعد درج البرج كل مساء عند الغروب، ويشعل المصباح الكبير، ثم يكتب سطراً واحداً في الدفتر.").unwrap();
        assert_eq!((arabic.code, arabic.script.as_str()), ("ara", "Arabic"));
        assert_eq!(detect(""), None);
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
