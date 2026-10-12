use std::collections::{HashMap, HashSet};
use std::io;

/// How many pages are held back to learn a document's running headers and footers.
const WINDOW_PAGES: usize = 8;

/// A page's text with its running header and footer taken out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub text: String,
    pub header: Option<String>,
    pub footer: Option<String>,
}

/// Finds the lines repeated at the top or bottom of most pages (running headers and footers,
/// page numbers) and moves them out of the page text. It learns them from the first pages, held
/// back meanwhile, so the output still streams.
pub struct RunningLines<'a, T> {
    emit: &'a mut dyn FnMut(T, Page) -> io::Result<()>,
    held: Vec<(T, String)>,
    learned: Option<Patterns>,
}

#[derive(Debug, Default)]
struct Patterns {
    headers: HashSet<String>,
    footers: HashSet<String>,
}

impl<'a, T> RunningLines<'a, T> {
    /// `emit` receives each page, in order, with what it was pushed with. Unless `enabled`,
    /// pages pass through unchanged.
    pub fn new(emit: &'a mut dyn FnMut(T, Page) -> io::Result<()>, enabled: bool) -> Self {
        Self {
            emit,
            held: Vec::new(),
            learned: (!enabled).then(Patterns::default),
        }
    }

    pub fn push(&mut self, with: T, text: String) -> io::Result<()> {
        if let Some(patterns) = &self.learned {
            return (self.emit)(with, split(text, patterns));
        }
        self.held.push((with, text));
        if self.held.len() == WINDOW_PAGES {
            self.learn()?;
        }
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        if self.learned.is_none() {
            self.learn()?;
        }
        Ok(())
    }

    fn learn(&mut self) -> io::Result<()> {
        let patterns = learn(self.held.iter().map(|(_, text)| text.as_str()));
        for (with, text) in std::mem::take(&mut self.held) {
            (self.emit)(with, split(text, &patterns))?;
        }
        self.learned = Some(patterns);
        Ok(())
    }
}

/// A line as it is compared across pages: digits stand for any number, so "Page 3" and
/// "Page 14" match, and case and spacing do not matter.
fn pattern(line: &str) -> String {
    let mut pattern = String::with_capacity(line.len());
    let mut in_number = false;
    for character in line.trim().chars() {
        if character.is_ascii_digit() {
            if !in_number {
                pattern.push('#');
            }
            in_number = true;
            continue;
        }
        in_number = false;
        if character.is_whitespace() {
            if !pattern.ends_with(' ') {
                pattern.push(' ');
            }
        } else {
            pattern.extend(character.to_lowercase());
        }
    }
    pattern
}

fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

fn last_line(text: &str) -> Option<&str> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
}

/// The first and last lines that recur on at least half of the pages, and on two at least.
fn learn<'t>(pages: impl Iterator<Item = &'t str>) -> Patterns {
    let mut headers: HashMap<String, usize> = HashMap::new();
    let mut footers: HashMap<String, usize> = HashMap::new();
    let mut count: usize = 0;
    for page in pages {
        count += 1;
        if let Some(line) = first_line(page) {
            *headers.entry(pattern(line)).or_default() += 1;
        }
        if let Some(line) = last_line(page) {
            *footers.entry(pattern(line)).or_default() += 1;
        }
    }
    let threshold = count.div_ceil(2).max(2);
    let recurring = |counts: HashMap<String, usize>| {
        counts
            .into_iter()
            .filter(|(_, pages)| *pages >= threshold)
            .map(|(pattern, _)| pattern)
            .collect()
    };
    Patterns {
        headers: recurring(headers),
        footers: recurring(footers),
    }
}

fn split(text: String, patterns: &Patterns) -> Page {
    let mut lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|line| !line.trim().is_empty());
    let header = start
        .filter(|&index| patterns.headers.contains(&pattern(lines[index])))
        .map(|index| lines.remove(index).trim().to_string());
    let end = lines.iter().rposition(|line| !line.trim().is_empty());
    let footer = end
        .filter(|&index| patterns.footers.contains(&pattern(lines[index])))
        .map(|index| lines.remove(index).trim().to_string());
    if header.is_none() && footer.is_none() {
        return Page {
            text,
            header,
            footer,
        };
    }
    Page {
        text: lines.join("\n").trim_matches('\n').to_string(),
        header,
        footer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(pages: &[&str]) -> Vec<Page> {
        let mut out = Vec::new();
        let mut emit = |index: usize, page: Page| {
            assert_eq!(index, out.len());
            out.push(page);
            Ok(())
        };
        let mut running = RunningLines::new(&mut emit, true);
        for (index, page) in pages.iter().enumerate() {
            running.push(index, page.to_string()).unwrap();
        }
        running.finish().unwrap();
        out
    }

    #[test]
    fn running_headers_and_page_numbers_move_out_of_the_text() {
        let pages: Vec<String> = (1..=12)
            .map(|number| {
                format!("Annual Report 2024\n\nBody of page {number}.\n\nPage {number} of 12")
            })
            .collect();
        let pages: Vec<&str> = pages.iter().map(String::as_str).collect();
        let out = run(&pages);
        assert_eq!(out.len(), 12);
        for (number, page) in (1..=12).zip(&out) {
            assert_eq!(page.text, format!("Body of page {number}."));
            assert_eq!(page.header.as_deref(), Some("Annual Report 2024"));
            assert_eq!(page.footer, Some(format!("Page {number} of 12")));
        }
    }

    #[test]
    fn lines_that_do_not_recur_stay() {
        let out = run(&[
            "Introduction\nFirst page.",
            "Methods\nSecond page.",
            "Results\nThird.",
        ]);
        assert!(out
            .iter()
            .all(|page| page.header.is_none() && page.footer.is_none()));
        assert_eq!(out[0].text, "Introduction\nFirst page.");
        let single = run(&["Title\nOnly page\nPage 1"]);
        assert_eq!(single[0].header, None);
        assert_eq!(single[0].footer, None);
    }
}
