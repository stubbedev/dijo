//! The bytes of an RTF document as `TextExtractor` reads them: through a
//! `PushbackInputStream` that takes back two bytes. Pushing back the end of the input, as
//! `TextExtractor` does when a control word ends the document, pushes back the byte 0xFF, which
//! is then read like any other.

use crate::formats::Failure;

const PUSHBACK: usize = 2;
const END_PUSHED_BACK: u8 = 0xFF;

pub(super) struct Input<'a> {
    bytes: &'a [u8],
    at: usize,
    pushed_back: [u8; PUSHBACK],
    pushed: usize,
}

impl<'a> Input<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            at: 0,
            pushed_back: [0; PUSHBACK],
            pushed: 0,
        }
    }

    /// The next byte, or `None` at the end.
    pub(super) fn read(&mut self) -> Option<u8> {
        if self.pushed > 0 {
            self.pushed -= 1;
            return Some(self.pushed_back[self.pushed]);
        }
        let byte = self.bytes.get(self.at).copied();
        self.at += usize::from(byte.is_some());
        byte
    }

    /// `PushbackInputStream.unread`, which fails when two bytes are already pushed back.
    pub(super) fn unread(&mut self, byte: Option<u8>) -> Result<(), Failure> {
        if self.pushed == PUSHBACK {
            return Err(Failure::Malformed(format!(
                "The RTF reader cannot push back a third byte at offset {}",
                self.at
            )));
        }
        self.pushed_back[self.pushed] = byte.unwrap_or(END_PUSHED_BACK);
        self.pushed += 1;
        Ok(())
    }

    /// The next `length` bytes, or, as `IOUtils.readFully` fails when fewer are left, how many
    /// were, all of them read.
    pub(super) fn take(&mut self, length: usize) -> Result<Vec<u8>, usize> {
        let available = self.pushed + (self.bytes.len() - self.at);
        if length > available {
            self.pushed = 0;
            self.at = self.bytes.len();
            return Err(available);
        }
        let mut taken = Vec::with_capacity(length);
        while taken.len() < length && self.pushed > 0 {
            taken.extend(self.read());
        }
        let rest = length - taken.len();
        taken.extend_from_slice(&self.bytes[self.at..self.at + rest]);
        self.at += rest;
        Ok(taken)
    }

    /// Skips `length` bytes, as `IOUtils.skipFully` does, failing when fewer are left.
    pub(super) fn skip(&mut self, length: usize) -> Result<(), Failure> {
        let available = self.pushed + (self.bytes.len() - self.at);
        if length > available {
            self.pushed = 0;
            self.at = self.bytes.len();
            return Err(Failure::Malformed(format!(
                "The RTF data of a \\bin control word ends after {available} of its {length} bytes"
            )));
        }
        let from_pushed = length.min(self.pushed);
        self.pushed -= from_pushed;
        self.at += length - from_pushed;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pushed_back_bytes_last_in_first_out() {
        let mut input = Input::new(b"ab");
        assert_eq!(input.read(), Some(b'a'));
        input.unread(Some(b'x')).unwrap();
        input.unread(Some(b'y')).unwrap();
        assert!(input.unread(Some(b'z')).is_err());
        assert_eq!(input.read(), Some(b'y'));
        assert_eq!(input.read(), Some(b'x'));
        assert_eq!(input.read(), Some(b'b'));
        assert_eq!(input.read(), None);
        input.unread(None).unwrap();
        assert_eq!(input.read(), Some(0xFF));
        assert_eq!(input.read(), None);
    }

    #[test]
    fn takes_and_skips_through_pushed_back_bytes() {
        let mut input = Input::new(b"abcdef");
        input.read();
        input.unread(Some(b'a')).unwrap();
        assert_eq!(input.take(3), Ok(b"abc".to_vec()));
        input.unread(Some(b'c')).unwrap();
        input.skip(2).unwrap();
        assert_eq!(input.read(), Some(b'e'));
        assert!(input.skip(2).is_err());
        assert_eq!(input.read(), None);
        let mut input = Input::new(b"abc");
        input.read();
        input.unread(Some(b'a')).unwrap();
        assert_eq!(input.take(4), Err(3));
        assert_eq!(input.read(), None);
    }
}
