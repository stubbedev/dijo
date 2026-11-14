//! The heading level of paragraph styles that are a built-in heading style or are based on one,
//! as the AbiWord and Scribus parsers resolve them through each style's parent.

use std::collections::HashMap;

/// The elements of the heading levels, the first level's first.
pub(crate) const HEADINGS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];

/// The heading level of each paragraph style of a document: of a built-in heading style, named
/// one of its prefixes and a level of [`HEADINGS`], or of a style based on one.
#[derive(Debug)]
pub(crate) struct StyleHeadings {
    prefixes: &'static [&'static str],
    levels: HashMap<String, usize>,
}

impl StyleHeadings {
    /// The heading levels of `styles`, the name of each and the style it is based on, an empty
    /// name for none, with the built-in heading styles named by `prefixes`; a cycle of styles is
    /// none.
    pub(crate) fn new(styles: &[(String, String)], prefixes: &'static [&'static str]) -> Self {
        let based_on: HashMap<&str, &str> = styles
            .iter()
            .map(|(name, base)| (name.as_str(), base.as_str()))
            .collect();
        let mut resolved: HashMap<&str, Option<usize>> = HashMap::with_capacity(styles.len());
        let mut chain = Vec::new();
        for (name, _) in styles {
            chain.clear();
            let mut current = name.as_str();
            let level = loop {
                if let Some(&level) = resolved.get(current) {
                    break level;
                }
                if let Some(level) = built_in(prefixes, current) {
                    break Some(level);
                }
                if chain.contains(&current) {
                    break None;
                }
                chain.push(current);
                match based_on.get(current) {
                    Some(&base) if !base.is_empty() => current = base,
                    Some(_) | None => break None,
                }
            };
            for &link in &chain {
                resolved.insert(link, level);
            }
        }
        let levels = resolved
            .into_iter()
            .filter_map(|(name, level)| Some((name.to_owned(), level?)))
            .collect();
        StyleHeadings { prefixes, levels }
    }

    /// The heading level of the style named `style`, which need not be one of the document's.
    pub(crate) fn level(&self, style: &str) -> Option<usize> {
        self.levels
            .get(style)
            .copied()
            .or_else(|| built_in(self.prefixes, style))
    }
}

/// The heading level of a built-in heading style named one of `prefixes` and a level.
fn built_in(prefixes: &[&str], style: &str) -> Option<usize> {
    prefixes
        .iter()
        .find_map(|prefix| style.strip_prefix(prefix))?
        .parse::<usize>()
        .ok()
        .filter(|level| (1..=HEADINGS.len()).contains(level))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIXES: &[&str] = &["Heading ", "Numbered Heading "];

    fn styles(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, base)| ((*name).to_owned(), (*base).to_owned()))
            .collect()
    }

    #[test]
    fn names_built_in_heading_styles_by_each_prefix() {
        let headings = StyleHeadings::new(&[], PREFIXES);
        assert_eq!(headings.level("Heading 1"), Some(1));
        assert_eq!(headings.level("Numbered Heading 6"), Some(6));
        assert_eq!(headings.level("Heading 7"), None);
        assert_eq!(headings.level("Heading 0"), None);
        assert_eq!(headings.level("Heading x"), None);
        assert_eq!(headings.level("Normal"), None);
        let only_heading = StyleHeadings::new(&[], &["Heading "]);
        assert_eq!(only_heading.level("Numbered Heading 2"), None);
    }

    #[test]
    fn takes_the_level_of_the_nearest_built_in_parent() {
        let headings = StyleHeadings::new(
            &styles(&[
                ("Chapter", "Heading 2"),
                ("Big Chapter", "Chapter"),
                ("Body", "Normal"),
                ("Normal", ""),
                ("Section", "Numbered Heading 3"),
                ("Heading 4", "Heading 1"),
            ]),
            PREFIXES,
        );
        assert_eq!(headings.level("Chapter"), Some(2));
        assert_eq!(headings.level("Big Chapter"), Some(2));
        assert_eq!(headings.level("Section"), Some(3));
        assert_eq!(headings.level("Heading 4"), Some(4));
        assert_eq!(headings.level("Body"), None);
        assert_eq!(headings.level("Normal"), None);
        assert_eq!(headings.level("Unknown"), None);
    }

    #[test]
    fn a_cycle_of_styles_is_no_heading() {
        let headings = StyleHeadings::new(
            &styles(&[("A", "B"), ("B", "A"), ("C", "A"), ("D", "Missing")]),
            PREFIXES,
        );
        for style in ["A", "B", "C", "D"] {
            assert_eq!(headings.level(style), None, "{style}");
        }
    }
}
