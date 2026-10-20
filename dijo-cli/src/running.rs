use std::io;

/// How many pages are held back at a time to learn a document's running headers and footers:
/// six of each side, so that a head of only the left or the right pages recurs on three.
const WINDOW_PAGES: usize = 12;
/// The most text held back meanwhile; with more, the pages held so far are learned from, so a
/// document with long text between or before its pages still streams.
const WINDOW_BYTES: usize = 16 << 20;
/// The most characters a typeset line holds. A longer line, such as a spreadsheet's first row,
/// was not read off a printed page and is compared exactly, which also bounds the cost of
/// comparing lines approximately.
const LINE_CHARS: usize = 200;

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
/// page numbers) and moves them out of the page text. It learns them from each window of pages,
/// held back meanwhile so the output still streams, and keeps what earlier windows taught: a book
/// whose heads begin after its front matter, or change from one part to the next, has them all
/// moved out.
pub struct RunningLines<'a, T> {
    emit: &'a mut dyn FnMut(T, Page) -> io::Result<()>,
    enabled: bool,
    held: Vec<(T, String, Placement)>,
    held_bytes: usize,
    learned: Patterns,
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
    headers: Vec<Vec<char>>,
    footers: Vec<Vec<char>>,
}

impl Patterns {
    fn extend(&mut self, other: Patterns) {
        for (known, new) in [
            (&mut self.headers, other.headers),
            (&mut self.footers, other.footers),
        ] {
            for pattern in new {
                if !recurs(&pattern, known) {
                    known.push(pattern);
                }
            }
        }
    }
}

impl<'a, T> RunningLines<'a, T> {
    /// `emit` receives each page, in order, with what it was pushed with. Unless `enabled`,
    /// pages pass through unchanged.
    pub fn new(emit: &'a mut dyn FnMut(T, Page) -> io::Result<()>, enabled: bool) -> Self {
        Self {
            emit,
            enabled,
            held: Vec::new(),
            held_bytes: 0,
            learned: Patterns::default(),
        }
    }

    pub fn push(&mut self, with: T, text: String, placement: Placement) -> io::Result<()> {
        if !self.enabled || (self.held.is_empty() && placement == Placement::Loose) {
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
        self.learn()
    }

    fn learn(&mut self) -> io::Result<()> {
        self.learned.extend(learn(
            self.held
                .iter()
                .map(|(_, text, placement)| (text.as_str(), *placement)),
        ));
        self.held_bytes = 0;
        for (with, text, placement) in std::mem::take(&mut self.held) {
            (self.emit)(with, split(text, placement, &self.learned))?;
        }
        Ok(())
    }
}

/// A line as it is compared across pages: digits stand for any number, so "Page 3" and
/// "Page 14" match, and case and spacing do not matter.
fn pattern(line: &str) -> Vec<char> {
    let mut pattern = Vec::with_capacity(line.len());
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
            if pattern.last() != Some(&' ') {
                pattern.push(' ');
            }
        } else {
            pattern.extend(character.to_lowercase());
        }
    }
    pattern
}

/// Whether two patterns are the same line, read alike but for at most one character in ten of
/// the shorter: OCR reads one running head as `Quixote de la Mancha`, `Ouixote dela Mancha` and
/// `Quixote de la Atancha`. A line of fewer than ten characters, such as a page number, or of
/// more than [`LINE_CHARS`], has to match exactly.
fn similar(one: &[char], other: &[char]) -> bool {
    let shorter = one.len().min(other.len());
    let bound = if shorter > LINE_CHARS {
        0
    } else {
        shorter / 10
    };
    if one.len().abs_diff(other.len()) > bound {
        return false;
    }
    if bound == 0 {
        return one == other;
    }
    within_edits(one, other, bound)
}

/// Whether the Levenshtein distance of `one` and `other` is at most `bound`, computed in the band
/// of the edit table that distance can reach and given up once a row of it exceeds `bound`.
fn within_edits(one: &[char], other: &[char], bound: usize) -> bool {
    let beyond = bound + 1;
    let mut previous: Vec<usize> = (0..=other.len()).map(|edits| edits.min(beyond)).collect();
    let mut current = vec![beyond; other.len() + 1];
    for (row, &character) in one.iter().enumerate() {
        let row = row + 1;
        let from = row.saturating_sub(bound).max(1);
        let to = (row + bound).min(other.len());
        current[from - 1] = if from == 1 { row.min(beyond) } else { beyond };
        let mut lowest = current[from - 1];
        for column in from..=to {
            let substitution = previous[column - 1] + usize::from(character != other[column - 1]);
            let edits = substitution
                .min(previous[column] + 1)
                .min(current[column - 1] + 1)
                .min(beyond);
            current[column] = edits;
            lowest = lowest.min(edits);
        }
        if to < other.len() {
            current[to + 1] = beyond;
        }
        if lowest > bound {
            return false;
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[other.len()] <= bound
}

fn recurs(pattern: &[char], learned: &[Vec<char>]) -> bool {
    learned.iter().any(|known| similar(known, pattern))
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

/// The first and last lines that recur, as [`similar`] lines, on at least half of the pages and
/// on two at least, or on at least half of the pages of one side and on three at least: a book
/// heads its left pages with its title and its right pages with the chapter's, and OCR misses
/// some of either.
fn learn<'t>(texts: impl Iterator<Item = (&'t str, Placement)>) -> Patterns {
    let mut headers = Vec::new();
    let mut footers = Vec::new();
    for (text, placement) in texts {
        let Placement::Page {
            first_line,
            last_line,
        } = placement
        else {
            continue;
        };
        if first_line {
            headers.push(first_line_of(text).map(pattern));
        }
        if last_line {
            footers.push(last_line_of(text).map(pattern));
        }
    }
    Patterns {
        headers: recurring(&headers),
        footers: recurring(&footers),
    }
}

fn recurring(lines: &[Option<Vec<char>>]) -> Vec<Vec<char>> {
    let mut learned: Vec<Vec<char>> = Vec::new();
    for pattern in lines.iter().flatten() {
        if recurs(pattern, &learned) {
            continue;
        }
        let on_most_pages = support(pattern, lines.iter()) >= lines.len().div_ceil(2).max(2);
        let on_most_of_a_side = [0, 1].into_iter().any(|side| {
            let pages = lines.len().saturating_sub(side).div_ceil(2);
            support(pattern, lines.iter().skip(side).step_by(2)) >= pages.div_ceil(2).max(3)
        });
        if on_most_pages || on_most_of_a_side {
            learned.push(pattern.clone());
        }
    }
    learned
}

fn support<'a>(pattern: &[char], lines: impl Iterator<Item = &'a Option<Vec<char>>>) -> usize {
    lines
        .flatten()
        .filter(|line| similar(pattern, line))
        .count()
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
fn take_first_line<'t>(text: &'t str, patterns: &[Vec<char>]) -> Option<(String, &'t str)> {
    let line = first_line_of(text)?;
    if !recurs(&pattern(line), patterns) {
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
fn take_last_line<'t>(text: &'t str, patterns: &[Vec<char>]) -> Option<(String, &'t str)> {
    let line = last_line_of(text)?;
    if !recurs(&pattern(line), patterns) {
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

    /// A line of text that no other page has.
    fn body(number: usize) -> String {
        let mut letters = String::new();
        let mut rest = number;
        loop {
            letters.insert(0, char::from(b'a' + (rest % 26) as u8));
            rest /= 26;
            if rest == 0 {
                return letters;
            }
        }
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
    fn heads_are_learned_after_front_matter_and_from_part_to_part() {
        let mut pages: Vec<String> = [
            "Cover",
            "Endpaper",
            "Title",
            "Licence",
            "Errata",
            "Dedication",
            "Prologue",
            "Sonnet",
            "Epigram",
            "Contents",
        ]
        .iter()
        .enumerate()
        .map(|(number, title)| format!("{title}\n{}", body(number)))
        .collect();
        pages.extend((10..24).map(|number| format!("Part One\n{}", body(number))));
        pages.extend((24..36).map(|number| format!("Part Two\n{}", body(number))));
        let pages: Vec<&str> = pages.iter().map(String::as_str).collect();
        let out = run(&pages);
        assert!(out[..10].iter().all(|page| page.header.is_none()));
        for (number, page) in (12..24).zip(&out[12..24]) {
            assert_eq!(page.header.as_deref(), Some("Part One"));
            assert_eq!(page.text, body(number));
        }
        assert!(out[24..]
            .iter()
            .all(|page| page.header.as_deref() == Some("Part Two")));
    }

    #[test]
    fn alternating_heads_are_learned_though_ocr_misreads_or_misses_some() {
        let heads = [
            "Tercera parte de don",
            "Quixote de la Mancha. 101",
            "Tercera parte de don",
            "Ouixote dela Mancha. 102",
            "Chapter text begins",
            "Quixote de la Mancha. 103",
            "Tercera parte de don",
            "Opening of a chapter",
            "Tercera parte de don",
            "Quixote de la Atancha. 105",
            "Tercera parte de don",
            "Quixote de la Mancha. 106",
        ];
        let pages: Vec<String> = heads
            .iter()
            .enumerate()
            .map(|(number, head)| format!("{head}\n{}", body(number)))
            .collect();
        let pages: Vec<&str> = pages.iter().map(String::as_str).collect();
        let out = run(&pages);
        for ((number, head), page) in heads.iter().enumerate().zip(&out) {
            if head.contains("parte") || head.contains("ancha") {
                assert_eq!(page.header.as_deref(), Some(*head));
                assert_eq!(page.text, body(number));
            } else {
                assert_eq!(page.header, None);
                assert_eq!(page.text, pages[number]);
            }
        }
    }

    #[test]
    fn a_word_closing_a_few_pages_of_a_side_is_not_a_footer() {
        let pages: Vec<String> = (1..=12)
            .map(|number| match number {
                1 | 3 => format!("Text of page {number} and\nque"),
                _ => format!("Text of page {number} and\nthe end of line {number}."),
            })
            .collect();
        let pages: Vec<&str> = pages.iter().map(String::as_str).collect();
        assert!(run(&pages).iter().all(|page| page.footer.is_none()));
    }

    #[test]
    fn only_lines_of_ten_characters_or_more_match_approximately() {
        let similar = |one: &str, other: &str| super::similar(&pattern(one), &pattern(other));
        assert!(similar(
            "Quixote de la Mancha. 7",
            "Ouixote dela Mancha. 12"
        ));
        assert!(!similar("Tercera parte de don", "Quarta parte de don"));
        assert!(similar("PROLOGO.", "Prologo."));
        assert!(!similar("PROLOGO.", "-PROLOGO."));
        assert!(!similar("Page 3", "Pag 3"));
    }

    #[test]
    fn edits_are_counted_as_levenshtein_counts_them() {
        fn levenshtein(one: &[char], other: &[char]) -> usize {
            let mut previous: Vec<usize> = (0..=other.len()).collect();
            for (row, character) in one.iter().enumerate() {
                let mut current = vec![row + 1];
                for (column, other_character) in other.iter().enumerate() {
                    current.push(
                        (previous[column] + usize::from(character != other_character))
                            .min(previous[column + 1] + 1)
                            .min(current[column] + 1),
                    );
                }
                previous = current;
            }
            previous[other.len()]
        }
        let words: Vec<Vec<char>> = (0..6)
            .flat_map(|length| {
                (0..1usize << length).map(move |bits| {
                    (0..length)
                        .map(|position| if bits >> position & 1 == 1 { 'a' } else { 'b' })
                        .collect()
                })
            })
            .collect();
        for one in &words {
            for other in &words {
                let distance = levenshtein(one, other);
                for bound in 1..=4 {
                    if one.len().abs_diff(other.len()) <= bound {
                        assert_eq!(
                            within_edits(one, other, bound),
                            distance <= bound,
                            "{one:?} {other:?} within {bound}"
                        );
                    }
                }
            }
        }
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
