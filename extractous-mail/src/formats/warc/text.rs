//! The byte buffer jwarc's WARC and HTTP machines push header text into: a position that
//! restarts at each field while the bytes behind it stay, and the end of the last text seen,
//! which a folded line and a header value are cut at even when it lies past the position.

#[derive(Debug, Default)]
pub(super) struct TextBuffer {
    bytes: Vec<u8>,
    position: usize,
    end_of_text: usize,
}

impl TextBuffer {
    /// `push`.
    pub(super) fn push(&mut self, byte: u8) {
        if self.position < self.bytes.len() {
            self.bytes[self.position] = byte;
        } else {
            self.bytes.push(byte);
        }
        self.position += 1;
    }

    /// The bytes pushed since the last restart.
    pub(super) fn pushed(&self) -> &[u8] {
        &self.bytes[..self.position]
    }

    /// The bytes up to the end of the last text, `new String(buf, 0, endOfText, …)`.
    pub(super) fn text(&self) -> &[u8] {
        &self.bytes[..self.end_of_text]
    }

    /// `bufPos = 0`.
    pub(super) fn restart(&mut self) {
        self.position = 0;
    }

    /// `endOfText = bufPos`.
    pub(super) fn end_text(&mut self) {
        self.end_of_text = self.position;
    }

    /// `endOfText = 0`.
    pub(super) fn forget_text(&mut self) {
        self.end_of_text = 0;
    }

    /// The `fold` action: a continuation line joins the text before it with one space.
    pub(super) fn fold(&mut self) {
        if self.position > 0 {
            self.position = self.end_of_text;
            self.push(b' ');
        }
    }
}
