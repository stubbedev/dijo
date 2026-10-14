use serde::Serialize;

mod charsoup;
mod groups;

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

/// Regional languages, dialects and historical and constructed languages that CharSoup takes
/// national languages of Europe for (Zazaki for Turkish, Swiss German for German, Walloon for
/// French): their logits are lowered by [`REGIONAL_PRIOR`], a prior that a text is far more
/// likely to be in the national language.
const REGIONAL: [&str; 41] = [
    "arg", "avk", "bar", "bre", "cor", "cos", "csb", "diq", "dsb", "epo", "ext", "frr", "fry",
    "gla", "glv", "gsw", "hsb", "hyw", "ido", "ile", "ina", "jbo", "ksh", "lat", "lfn", "lim",
    "mwl", "nds", "olo", "pfl", "rue", "sgs", "stq", "szl", "vep", "vls", "vol", "vro", "wln",
    "xmf", "nah",
];
const REGIONAL_PRIOR: f32 = 2.0;
/// How much a group model's logits count against CharSoup's.
const GROUP_WEIGHT: f32 = 1.0;
/// How far below the leader's logit CharSoup's logit for a member of its group may count.
const GROUP_LOGIT_FLOOR: f32 = 2.0;
/// How much more a group model must prefer a member CharSoup does not score in the text's
/// script than the leader, since most short texts read the same in both (Croatian and Serbian).
const UNSCORED_MARGIN: f32 = 3.0;
const MASKED_LOGIT: f32 = -1e30;

/// The language of a text, or None when it has no letters or no language leads. CharSoup scores
/// it, regional languages get the [`REGIONAL`] prior, and when the leader belongs to a group of
/// closely related languages, the group's own model weighs in. A label with a private-use
/// orthography tag, such as `be-x-old` for Belarusian in the Taraškievica spelling, is reported
/// as its base language.
pub fn detect(text: &str) -> Option<DetectedLanguage> {
    let charsoup::Scores { mut logits, script } = charsoup::scores(text)?;
    let mut labels: Vec<&'static str> = charsoup::labels()
        .iter()
        .map(|label| label.split_once("-x-").map_or(*label, |(base, _)| base))
        .collect();
    merge_orthographies(&labels, &mut logits);
    for (logit, label) in logits.iter_mut().zip(&labels) {
        if REGIONAL.contains(label) {
            *logit -= REGIONAL_PRIOR;
        }
    }
    weigh_in_group(text, script, &mut labels, &mut logits);
    let (top, score, _, confidence) = charsoup::decide(&logits)?;
    Some(DetectedLanguage {
        code: labels[top],
        script: script.full_name(),
        confidence: f64::from(score),
        reliable: matches!(confidence, Confidence::High | Confidence::Medium),
    })
}

/// Keeps the best logit of a language written in more than one orthography on its first label
/// and masks the others.
fn merge_orthographies(labels: &[&str], logits: &mut [f32]) {
    for index in 0..labels.len() {
        let Some(first) = labels.iter().position(|label| *label == labels[index]) else {
            continue;
        };
        if first != index {
            logits[first] = logits[first].max(logits[index]);
            logits[index] = MASKED_LOGIT;
        }
    }
}

/// Rescores the members of the leader's group, each from its CharSoup logit (no further below
/// the leader's than [`GROUP_LOGIT_FLOOR`], or [`UNSCORED_MARGIN`] below it for a member CharSoup
/// does not score in this script) plus the group model's preference for it. The group's best
/// keeps the leader's logit: the group model decides between the members, not against the rest.
fn weigh_in_group(
    text: &str,
    script: unicode_script::Script,
    labels: &mut Vec<&'static str>,
    logits: &mut Vec<f32>,
) {
    let top = (0..logits.len())
        .max_by(|&a, &b| logits[a].total_cmp(&logits[b]))
        .expect("CharSoup has labels");
    let Some(group) = groups::group_of(script, labels[top]) else {
        return;
    };
    let Some(group_logits) = group.logits(text) else {
        return;
    };
    let leader = logits[top];
    let best = group_logits
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let indices: Vec<usize> = group
        .labels
        .iter()
        .map(|&label| {
            labels
                .iter()
                .position(|known| *known == label)
                .unwrap_or_else(|| {
                    labels.push(label);
                    logits.push(MASKED_LOGIT);
                    labels.len() - 1
                })
        })
        .collect();
    let combined: Vec<f32> = indices
        .iter()
        .zip(&group.scored)
        .zip(&group_logits)
        .map(|((&index, &scored), &group_logit)| {
            let base = if scored {
                logits[index].max(leader - GROUP_LOGIT_FLOOR)
            } else {
                leader - UNSCORED_MARGIN
            };
            base + GROUP_WEIGHT * (group_logit - best)
        })
        .collect();
    let shift = leader - combined.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    for (&index, &score) in indices.iter().zip(&combined) {
        logits[index] = score + shift;
    }
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

    const RUN: usize = 5;
    /// The least mean, over the fixture's languages, of the share of sentences alone and in
    /// runs of [`RUN`] detected as their language.
    const LEAST_MEAN_ALONE: f64 = 0.86;
    const LEAST_MEAN_IN_RUNS: f64 = 0.98;

    /// The language a fixture label expects: its ISO 639-3 code, without a script suffix such
    /// as `-Latn`.
    fn expected(label: &str) -> &str {
        label.split_once('-').map_or(label, |(code, _)| code)
    }

    /// Tika's CharSoup answer, reported as this detector reports a language.
    fn tika(text: &str) -> Option<&'static str> {
        charsoup::detect(text).map(|found| {
            found
                .label
                .split_once("-x-")
                .map_or(found.label, |(base, _)| base)
        })
    }

    #[test]
    fn every_language_is_told_apart_at_least_as_well_as_by_tika() {
        let tatoeba = include_str!("../../tests/fixtures/language/tatoeba.tsv");
        let mut sentences: Vec<(&str, Vec<&str>)> = Vec::new();
        for line in tatoeba.lines() {
            let mut fields = line.splitn(3, '\t');
            let (Some(label), Some(_), Some(sentence)) =
                (fields.next(), fields.next(), fields.next())
            else {
                panic!("malformed fixture line {line:?}");
            };
            match sentences.iter_mut().find(|(known, _)| *known == label) {
                Some((_, list)) => list.push(sentence),
                None => sentences.push((label, vec![sentence])),
            }
        }
        let share =
            |texts: &[String], code: &str, detector: &dyn Fn(&str) -> Option<&'static str>| {
                let right = texts
                    .iter()
                    .filter(|text| detector(text) == Some(code))
                    .count();
                right as f64 / texts.len() as f64
            };
        let ours = |text: &str| detect(text).map(|found| found.code);
        let mut failures = Vec::new();
        let (mut sum_alone, mut sum_in_runs) = (0.0, 0.0);
        for (label, list) in &sentences {
            let code = expected(label);
            let alone: Vec<String> = list.iter().map(|s| s.to_string()).collect();
            let runs: Vec<String> = list
                .as_chunks::<RUN>()
                .0
                .iter()
                .map(|run| run.join(" "))
                .collect();
            let (ours_alone, tika_alone) = (share(&alone, code, &ours), share(&alone, code, &tika));
            let (ours_in_runs, tika_in_runs) =
                (share(&runs, code, &ours), share(&runs, code, &tika));
            sum_alone += ours_alone;
            sum_in_runs += ours_in_runs;
            if ours_alone < tika_alone || ours_in_runs < tika_in_runs {
                failures.push(format!(
                    "{label}: {ours_alone:.3} of sentences (Tika {tika_alone:.3}), \
                     {ours_in_runs:.3} of runs of {RUN} (Tika {tika_in_runs:.3})"
                ));
            }
        }
        let count = sentences.len() as f64;
        let (mean_alone, mean_in_runs) = (sum_alone / count, sum_in_runs / count);
        if mean_alone < LEAST_MEAN_ALONE || mean_in_runs < LEAST_MEAN_IN_RUNS {
            failures.push(format!(
                "mean {mean_alone:.3} of sentences (least {LEAST_MEAN_ALONE}), \
                 {mean_in_runs:.3} of runs (least {LEAST_MEAN_IN_RUNS})"
            ));
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// Compares Tika's CharSoup with this detector on LANGUAGE_EVALUATION, a TSV of the
    /// expected ISO 639-3 code (with an optional `-<script>` suffix) and a text per line, and
    /// prints each language's accuracy for both.
    #[test]
    #[ignore = "needs an evaluation set; `just language-accuracy` runs it"]
    fn accuracy_against_tika() {
        let path = std::env::var_os("LANGUAGE_EVALUATION")
            .expect("LANGUAGE_EVALUATION names the evaluation TSV");
        let data = std::fs::read_to_string(&path).unwrap();
        let mut results: std::collections::BTreeMap<&str, (usize, usize, usize)> =
            std::collections::BTreeMap::new();
        for line in data.lines() {
            let (gold, text) = line.split_once('\t').expect("a code and a text");
            let code = expected(gold);
            let tika = tika(text);
            let ours = detect(text).map(|found| found.code);
            let entry = results.entry(gold).or_default();
            entry.0 += 1;
            entry.1 += usize::from(tika == Some(code));
            entry.2 += usize::from(ours == Some(code));
        }
        let (mut tika_sum, mut ours_sum) = (0.0, 0.0);
        println!("| Language | Texts | Tika (CharSoup) | extractous |\n|---|---|---|---|");
        for (gold, (total, tika, ours)) in &results {
            let (tika, ours) = (*tika as f64 / *total as f64, *ours as f64 / *total as f64);
            tika_sum += tika;
            ours_sum += ours;
            println!("| {gold} | {total} | {tika:.3} | {ours:.3} |");
        }
        let count = results.len() as f64;
        println!(
            "| **Mean** | | {:.4} | {:.4} |",
            tika_sum / count,
            ours_sum / count
        );
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
