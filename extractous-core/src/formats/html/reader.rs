//! jsoup's `CharacterReader`: the decoded text, in UTF-16 units as Java holds it, read through a
//! window of 2,048 units that is refilled once half of it is consumed. The window shows: jsoup
//! looks ahead only within it, and some reads stop at its end, so the port keeps it exactly.
//! Reading past the end yields `EOF`, U+FFFF, which jsoup cannot tell from a U+FFFF in the text.

/// What jsoup's reader returns at the end of the input: `(char) -1`.
pub(super) const EOF: u16 = 0xFFFF;
const BUFFER_SIZE: usize = 2048;
const REFILL_POINT: usize = BUFFER_SIZE / 2;
const REWIND_LIMIT: usize = 1024;

/// How the window is filled from the text: a `StringReader` fills it to the last unit, while
/// jsoup's stream reader, decoding bytes, stops short of a surrogate pair it cannot fit whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    #[cfg_attr(not(test), expect(dead_code))]
    String,
    Stream,
}

pub(super) struct CharacterReader<'a> {
    text: &'a [u16],
    source: Source,
    /// Where in the text the window starts (`consumed`).
    start: usize,
    pos: usize,
    len: usize,
    fill_point: usize,
    mark: Option<usize>,
    read_fully: bool,
}

pub(super) fn is_high_surrogate(unit: u16) -> bool {
    (0xD800..0xDC00).contains(&unit)
}

fn is_low_surrogate(unit: u16) -> bool {
    (0xDC00..0xE000).contains(&unit)
}

pub(super) fn is_ascii_letter(unit: u16) -> bool {
    matches!(unit, 0x41..=0x5A | 0x61..=0x7A)
}

pub(super) fn is_digit(unit: u16) -> bool {
    (0x30..=0x39).contains(&unit)
}

fn is_hex_digit(unit: u16) -> bool {
    is_digit(unit) || matches!(unit, 0x41..=0x46 | 0x61..=0x66)
}

/// Java's `Character.toUpperCase(char)`, for the units whose simple mapping is one unit.
pub(super) fn java_upper(unit: u16) -> u16 {
    single_mapping(unit, |c| c.to_uppercase().collect())
}

/// Java's `Character.toLowerCase(char)`, for the units whose simple mapping is one unit.
pub(super) fn java_lower(unit: u16) -> u16 {
    single_mapping(unit, |c| c.to_lowercase().collect())
}

fn single_mapping(unit: u16, map: impl Fn(char) -> Vec<char>) -> u16 {
    let Some(c) = char::from_u32(u32::from(unit)) else {
        return unit;
    };
    match map(c).as_slice() {
        [mapped] if u32::from(*mapped) <= 0xFFFF => *mapped as u16,
        _ => unit,
    }
}

/// Java's `String.equalsIgnoreCase` over UTF-16 units.
pub(super) fn equals_ignore_case(a: &[u16], b: &[u16]) -> bool {
    a.len() == b.len() && region_matches_ignore_case(a, b)
}

/// Java's `regionMatches(true, ...)` over the whole of `a` against the start of `b`.
fn region_matches_ignore_case(a: &[u16], b: &[u16]) -> bool {
    a.iter().zip(b).all(|(&x, &y)| {
        if x == y {
            return true;
        }
        let (ux, uy) = (java_upper(x), java_upper(y));
        ux == uy || java_lower(ux) == java_lower(uy)
    })
}

impl<'a> CharacterReader<'a> {
    pub(super) fn new(text: &'a [u16], source: Source) -> Self {
        let mut reader = CharacterReader {
            text,
            source,
            start: 0,
            pos: 0,
            len: 0,
            fill_point: 0,
            mark: None,
            read_fully: false,
        };
        reader.buffer_up();
        reader
    }

    fn buffer_up(&mut self) {
        if self.read_fully || self.pos < self.fill_point || self.mark.is_some() {
            return;
        }
        self.start += self.pos;
        self.len = self
            .len
            .checked_sub(self.pos)
            .unwrap_or_else(|| panic!("jsoup's reader consumed past its window while marked"));
        self.pos = 0;
        while self.len < BUFFER_SIZE {
            let next = self.start + self.len;
            let remaining = self.text.len() - next;
            if remaining == 0 {
                self.read_fully = true;
                break;
            }
            let want = BUFFER_SIZE - self.len;
            let mut read = want.min(remaining);
            if self.source == Source::Stream
                && read == want
                && is_high_surrogate(self.text[next + read - 1])
                && self
                    .text
                    .get(next + read)
                    .copied()
                    .is_some_and(is_low_surrogate)
            {
                read -= 1;
            }
            if read == 0 {
                break;
            }
            self.len += read;
        }
        self.fill_point = self.len.min(REFILL_POINT);
    }

    fn window(&self) -> &'a [u16] {
        &self.text[self.start..self.start + self.len]
    }

    fn unit(&self, index: usize) -> u16 {
        self.text[self.start + index]
    }

    pub(super) fn mark(&mut self) {
        if self.len - self.pos < REWIND_LIMIT {
            self.fill_point = 0;
        }
        self.buffer_up();
        self.mark = Some(self.pos);
    }

    pub(super) fn unmark(&mut self) {
        self.mark = None;
    }

    pub(super) fn rewind_to_mark(&mut self) {
        let mark = self
            .mark
            .take()
            .unwrap_or_else(|| panic!("jsoup's reader rewound without a mark"));
        self.pos = mark;
    }

    pub(super) fn is_empty(&mut self) -> bool {
        self.buffer_up();
        self.pos >= self.len
    }

    pub(super) fn current(&mut self) -> u16 {
        self.buffer_up();
        if self.pos >= self.len {
            EOF
        } else {
            self.unit(self.pos)
        }
    }

    pub(super) fn consume(&mut self) -> u16 {
        self.buffer_up();
        let unit = if self.pos >= self.len {
            EOF
        } else {
            self.unit(self.pos)
        };
        self.pos += 1;
        unit
    }

    pub(super) fn unconsume(&mut self) {
        assert!(self.pos >= 1, "jsoup's reader unconsumed past its window");
        self.pos -= 1;
    }

    pub(super) fn advance(&mut self) {
        self.pos += 1;
    }

    fn next_index_of(&mut self, unit: u16) -> Option<usize> {
        self.buffer_up();
        self.window()[self.pos.min(self.len)..]
            .iter()
            .position(|&u| u == unit)
    }

    fn next_index_of_seq(&mut self, seq: &[u16]) -> Option<usize> {
        self.buffer_up();
        let window = self.window();
        let first = seq[0];
        let mut offset = self.pos;
        while offset < self.len {
            if first != window[offset] {
                offset += 1;
                while offset < self.len && first != window[offset] {
                    offset += 1;
                }
            }
            let mut i = offset + 1;
            let last = i + seq.len() - 1;
            if offset < self.len && last <= self.len {
                let mut j = 1;
                while i < last && seq[j] == window[i] {
                    i += 1;
                    j += 1;
                }
                if i == last {
                    return Some(offset - self.pos);
                }
            }
            offset += 1;
        }
        None
    }

    fn take(&mut self, count: usize) -> &'a [u16] {
        let window = self.window();
        let taken = &window[self.pos..self.pos + count];
        self.pos += count;
        taken
    }

    pub(super) fn consume_to(&mut self, unit: u16) -> &'a [u16] {
        match self.next_index_of(unit) {
            Some(offset) => self.take(offset),
            None => self.consume_to_end(),
        }
    }

    /// Reads up to `seq`, or, when the window does not hold it, all but the last units of the
    /// window that could start it.
    pub(super) fn consume_to_seq(&mut self, seq: &[u16]) -> &'a [u16] {
        if let Some(offset) = self.next_index_of_seq(seq) {
            return self.take(offset);
        }
        if self.len - self.pos < seq.len() {
            return self.consume_to_end();
        }
        let end = self.len - seq.len() + 1;
        self.take(end - self.pos)
    }

    pub(super) fn consume_matching(
        &mut self,
        test: impl Fn(u16) -> bool,
        max: Option<usize>,
    ) -> &'a [u16] {
        self.buffer_up();
        let window = self.window();
        let start = self.pos;
        let mut pos = start;
        while pos < self.len && max.is_none_or(|max| pos - start < max) && test(window[pos]) {
            pos += 1;
        }
        self.consume_range(start, pos)
    }

    pub(super) fn consume_to_any(&mut self, units: &[u16]) -> &'a [u16] {
        self.buffer_up();
        let window = self.window();
        let start = self.pos;
        let mut pos = start;
        while pos < self.len && !units.contains(&window[pos]) {
            pos += 1;
        }
        self.consume_range(start, pos)
    }

    fn consume_range(&mut self, start: usize, pos: usize) -> &'a [u16] {
        self.pos = pos;
        &self.window()[start..pos]
    }

    pub(super) fn consume_data(&mut self) -> &'a [u16] {
        self.consume_to_any(&[u16::from(b'&'), u16::from(b'<'), 0])
    }

    pub(super) fn consume_attribute_quoted(&mut self, single: bool) -> &'a [u16] {
        let quote = if single { b'\'' } else { b'"' };
        self.consume_to_any(&[0, u16::from(b'&'), u16::from(quote)])
    }

    pub(super) fn consume_raw_data(&mut self) -> &'a [u16] {
        self.consume_to_any(&[u16::from(b'<'), 0])
    }

    pub(super) fn consume_tag_name(&mut self) -> &'a [u16] {
        self.consume_to_any(&[9, 10, 13, 12, 32, u16::from(b'/'), u16::from(b'>')])
    }

    pub(super) fn consume_to_end(&mut self) -> &'a [u16] {
        self.buffer_up();
        let start = self.pos;
        let end = self.len;
        self.consume_range(start, end)
    }

    pub(super) fn consume_letter_sequence(&mut self) -> &'a [u16] {
        self.consume_matching(is_ascii_letter, None)
    }

    pub(super) fn consume_letter_then_digit_sequence(&mut self) -> &'a [u16] {
        self.buffer_up();
        let window = self.window();
        let start = self.pos;
        while self.pos < self.len && is_ascii_letter(window[self.pos]) {
            self.pos += 1;
        }
        while self.pos < self.len && is_digit(window[self.pos]) {
            self.pos += 1;
        }
        &window[start..self.pos]
    }

    pub(super) fn consume_hex_sequence(&mut self) -> &'a [u16] {
        self.consume_matching(is_hex_digit, None)
    }

    pub(super) fn consume_digit_sequence(&mut self) -> &'a [u16] {
        self.consume_matching(is_digit, None)
    }

    pub(super) fn matches(&mut self, unit: u16) -> bool {
        !self.is_empty() && self.unit(self.pos) == unit
    }

    pub(super) fn matches_seq(&mut self, seq: &str) -> bool {
        self.buffer_up();
        let seq: Vec<u16> = seq.encode_utf16().collect();
        if seq.len() > self.len - self.pos.min(self.len) {
            return false;
        }
        self.window()[self.pos..self.pos + seq.len()] == seq[..]
    }

    pub(super) fn matches_ignore_case(&mut self, seq: &str) -> bool {
        self.buffer_up();
        let seq: Vec<u16> = seq.encode_utf16().collect();
        if seq.len() > self.len - self.pos.min(self.len) {
            return false;
        }
        seq.iter()
            .zip(&self.window()[self.pos..])
            .all(|(&scan, &target)| scan == target || java_upper(scan) == java_upper(target))
    }

    pub(super) fn matches_any(&mut self, units: &[u16]) -> bool {
        if self.is_empty() {
            return false;
        }
        units.contains(&self.unit(self.pos))
    }

    pub(super) fn matches_ascii_alpha(&mut self) -> bool {
        !self.is_empty() && is_ascii_letter(self.unit(self.pos))
    }

    pub(super) fn matches_digit(&mut self) -> bool {
        !self.is_empty() && is_digit(self.unit(self.pos))
    }

    pub(super) fn match_consume(&mut self, seq: &str) -> bool {
        if self.matches_seq(seq) {
            self.pos += seq.len();
            true
        } else {
            false
        }
    }

    pub(super) fn match_consume_ignore_case(&mut self, seq: &str) -> bool {
        if self.matches_ignore_case(seq) {
            self.pos += seq.len();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn reads_through_a_window() {
        let text = units(&format!("{}<b>", "a".repeat(3000)));
        let mut reader = CharacterReader::new(&text, Source::String);
        assert_eq!(reader.consume_data().len(), 2048);
        assert_eq!(reader.consume_data().len(), 3000 - 2048);
        assert!(reader.matches(u16::from(b'<')));
        assert_eq!(reader.consume(), u16::from(b'<'));
        assert_eq!(reader.consume_tag_name(), units("b"));
        assert_eq!(reader.consume(), u16::from(b'>'));
        assert!(reader.is_empty());
        assert_eq!(reader.consume(), EOF);
    }

    #[test]
    fn keeps_a_surrogate_pair_whole_when_decoding() {
        let text = units(&format!("{}\u{1F600}x", "a".repeat(2047)));
        let mut stream = CharacterReader::new(&text, Source::Stream);
        assert_eq!(stream.consume_data().len(), 2047);
        let mut string = CharacterReader::new(&text, Source::String);
        assert_eq!(string.consume_data().len(), 2048);
    }

    #[test]
    fn compares_case_as_java_does() {
        assert!(equals_ignore_case(&units("SCRIPT"), &units("script")));
        assert!(equals_ignore_case(&units("ſcrıpt"), &units("SCRIPT")));
        let text = units("doctype");
        let mut reader = CharacterReader::new(&text, Source::String);
        assert!(reader.match_consume_ignore_case("DOCTYPE"));
    }
}
