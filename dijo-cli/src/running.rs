use std::collections::{HashMap, HashSet};
use std::io;

/// How many pages are held back to learn a document's running headers and footers.
const WINDOW_PAGES: usize = 8;
/// The most text held back meanwhile; with more, the pages held so far are learned from, so a
/// document with long text between or before its pages still streams.
const WINDOW_BYTES: usize = 16 << 20;

/// A page's text with its running header and footer taken out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub text: String,
    pub header: Option<String>,
    pub footer: Option<String>,
}

impl Page {
    fn unchanged(text: String) -> Self {
        Self {
            text,
            header: None,
            footer: None,
        }
    }
}

/// Finds the lines repeated at the top or bottom of most pages (running headers and footers,
/// page numbers) and moves them out of the page text. It learns them from the first pages, held
/// back meanwhile, so the output still streams.
pub struct RunningLines<'a, T> {
    emit: &'a mut dyn FnMut(T, Page) -> io::Result<()>,
    held: Vec<(T, String, Placement)>,
    held_bytes: usize,
    learned: Option<Patterns>,
}

/// Whether text is a page's, or lies outside every page, such as the bookmarks Tika appends to
/// a PDF. Only pages have running lines. A long page comes in pieces, and only the piece with a
/// page's first line can hold its header, only the one with its last line its footer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Page { first_line: bool, last_line: bool },
    Loose,
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
            held_bytes: 0,
            learned: (!enabled).then(Patterns::default),
        }
    }

    pub fn push(&mut self, with: T, text: String, placement: Placement) -> io::Result<()> {
        if let Some(patterns) = &self.learned {
            return (self.emit)(with, split(text, placement, patterns));
        }
        if self.held.is_empty() && placement == Placement::Loose {
            return (self.emit)(with, Page::unchanged(text));
        }
        self.held_bytes += text.len();
        self.held.push((with, text, placement));
        let pages = self
            .held
            .iter()
            .filter(|(_, _, placement)| {
                matches!(
                    placement,
                    Placement::Page {
                        last_line: true,
                        ..
                    }
                )
            })
            .count();
        if pages == WINDOW_PAGES || self.held_bytes >= WINDOW_BYTES {
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
        let patterns = learn(
            self.held
                .iter()
                .map(|(_, text, placement)| (text.as_str(), *placement)),
        );
        for (with, text, placement) in std::mem::take(&mut self.held) {
            (self.emit)(with, split(text, placement, &patterns))?;
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

fn first_line_of(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

fn last_line_of(text: &str) -> Option<&str> {
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
}

/// The first and last lines that recur on at least half of the pages, and on two at least.
fn learn<'t>(texts: impl Iterator<Item = (&'t str, Placement)>) -> Patterns {
    let mut headers: HashMap<String, usize> = HashMap::new();
    let mut footers: HashMap<String, usize> = HashMap::new();
    let mut count: usize = 0;
    for (text, placement) in texts {
        let Placement::Page {
            first_line,
            last_line,
        } = placement
        else {
            continue;
        };
        if first_line {
            if let Some(line) = first_line_of(text) {
                *headers.entry(pattern(line)).or_default() += 1;
            }
        }
        if last_line {
            count += 1;
            if let Some(line) = last_line_of(text) {
                *footers.entry(pattern(line)).or_default() += 1;
            }
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

fn split(text: String, placement: Placement, patterns: &Patterns) -> Page {
    let Placement::Page {
        first_line,
        last_line,
    } = placement
    else {
        return Page::unchanged(text);
    };
    let mut body = text.as_str();
    let header = first_line
        .then(|| take_first_line(body, &patterns.headers))
        .flatten()
        .map(|(line, rest)| {
            body = rest;
            line
        });
    let footer = last_line
        .then(|| take_last_line(body, &patterns.footers))
        .flatten()
        .map(|(line, rest)| {
            body = rest;
            line
        });
    // A page that is nothing but the recurring line, such as a document whose every page reads
    // the same, has that line as its text rather than as a running header or footer.
    if body.trim().is_empty() {
        return Page::unchanged(text);
    }
    Page {
        text: if header.is_none() && footer.is_none() {
            text
        } else {
            body.to_string()
        },
        header,
        footer,
    }
}

/// The first non-blank line of `text` when it matches one of `patterns`, and the text after it.
fn take_first_line<'t>(text: &'t str, patterns: &HashSet<String>) -> Option<(String, &'t str)> {
    let line = first_line_of(text)?;
    if !patterns.contains(&pattern(line)) {
        return None;
    }
    let end = line.as_ptr() as usize - text.as_ptr() as usize + line.len();
    let after = text[end..]
        .find('\n')
        .map_or(text.len(), |newline| end + newline);
    Some((
        line.to_string(),
        text[after..].trim_start_matches(['\r', '\n']),
    ))
}

/// The last non-blank line of `text` when it matches one of `patterns`, and the text before it.
fn take_last_line<'t>(text: &'t str, patterns: &HashSet<String>) -> Option<(String, &'t str)> {
    let line = last_line_of(text)?;
    if !patterns.contains(&pattern(line)) {
        return None;
    }
    let start = line.as_ptr() as usize - text.as_ptr() as usize;
    let before = text[..start].rfind('\n').map_or(0, |newline| newline + 1);
    Some((
        line.to_string(),
        text[..before].trim_end_matches(['\r', '\n']),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHOLE_PAGE: Placement = Placement::Page {
        first_line: true,
        last_line: true,
    };

    fn run(pages: &[&str]) -> Vec<Page> {
        let mut out = Vec::new();
        let mut emit = |index: usize, page: Page| {
            assert_eq!(index, out.len());
            out.push(page);
            Ok(())
        };
        let mut running = RunningLines::new(&mut emit, true);
        for (index, page) in pages.iter().enumerate() {
            running.push(index, page.to_string(), WHOLE_PAGE).unwrap();
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
    fn text_outside_pages_neither_teaches_nor_loses_lines() {
        let mut out = Vec::new();
        let mut emit = |index: usize, page: Page| {
            assert_eq!(index, out.len());
            out.push(page);
            Ok(())
        };
        let mut running = RunningLines::new(&mut emit, true);
        running
            .push(0, "Title\nFirst page.".to_string(), WHOLE_PAGE)
            .unwrap();
        running
            .push(1, "Title".to_string(), Placement::Loose)
            .unwrap();
        running.finish().unwrap();
        assert_eq!(out[0].text, "Title\nFirst page.");
        assert_eq!(out[0].header, None);
        assert_eq!(out[1].text, "Title");
    }

    #[test]
    fn a_page_in_pieces_loses_its_header_and_footer_once() {
        let mut out = Vec::new();
        let mut emit = |_: (), page: Page| {
            out.push(page);
            Ok(())
        };
        let mut running = RunningLines::new(&mut emit, true);
        for number in 1..=3 {
            let pieces = [
                (
                    format!("Annual Report\nFirst half of page {number}.\n"),
                    true,
                    false,
                ),
                ("Middle of the page.\n".to_string(), false, false),
                (format!("Second half.\nPage {number}"), false, true),
            ];
            for (text, first_line, last_line) in pieces {
                running
                    .push(
                        (),
                        text,
                        Placement::Page {
                            first_line,
                            last_line,
                        },
                    )
                    .unwrap();
            }
        }
        running.finish().unwrap();
        let pages: Vec<String> = out
            .chunks(3)
            .map(|pieces| pieces.iter().map(|piece| piece.text.as_str()).collect())
            .collect();
        assert_eq!(
            pages[1],
            "First half of page 2.\nMiddle of the page.\nSecond half."
        );
        assert_eq!(out[3].header.as_deref(), Some("Annual Report"));
        assert_eq!(out[5].footer.as_deref(), Some("Page 2"));
        assert!(out[4].header.is_none() && out[4].footer.is_none());
    }

    #[test]
    fn long_text_is_passed_on_before_the_document_ends() {
        let mut emitted = 0;
        let mut emit = |_: (), _: Page| {
            emitted += 1;
            Ok(())
        };
        let mut running = RunningLines::new(&mut emit, true);
        running.push((), "A page".to_string(), WHOLE_PAGE).unwrap();
        running
            .push((), "x".repeat(WINDOW_BYTES), Placement::Loose)
            .unwrap();
        running
            .push((), "more".to_string(), Placement::Loose)
            .unwrap();
        drop(running);
        assert_eq!(emitted, 3);
    }

    #[test]
    fn text_before_any_page_is_passed_on_at_once() {
        let mut out = Vec::new();
        let mut emit = |_: (), page: Page| {
            out.push(page.text);
            Ok(())
        };
        let mut running = RunningLines::new(&mut emit, true);
        running
            .push((), "A sheet".to_string(), Placement::Loose)
            .unwrap();
        running
            .push((), "Another sheet".to_string(), Placement::Loose)
            .unwrap();
        drop(running);
        assert_eq!(out, ["A sheet", "Another sheet"]);
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

    #[test]
    fn a_page_that_is_only_the_recurring_line_keeps_it() {
        let out = run(&["Product label", "Product label", "Product label"]);
        for page in &out {
            assert_eq!(page.text, "Product label");
            assert_eq!(page.header, None);
            assert_eq!(page.footer, None);
        }
        let mixed = run(&["Label\nFront.", "Label", "Label\nBack."]);
        assert_eq!(mixed[0].text, "Front.");
        assert_eq!(mixed[1].text, "Label");
        assert_eq!(mixed[2].text, "Back.");
    }
}
