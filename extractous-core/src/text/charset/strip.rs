//! A port of Tika's `HtmlByteStripper`: removes HTML and XML markup from bytes in any
//! ASCII-compatible charset, so that charset detection and junk scoring see the text. Tags,
//! comments, declarations and the bodies of `script` and `style` go; the values of attributes
//! people read (`alt`, `title`, `placeholder`, `aria-label`, `summary`, `label`) stay, after a
//! space; well-formed entities go too when asked.

const MAX_ENTITY_BODY_LEN: usize = 16;
const SCRIPT: &[u8] = b"script";
const STYLE: &[u8] = b"style";
const END_SCRIPT: &[u8] = b"</script";
const END_STYLE: &[u8] = b"</style";
const TEXT_ATTRS: [&[u8]; 6] = [
    b"alt",
    b"title",
    b"placeholder",
    b"aria-label",
    b"summary",
    b"label",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Text,
    Lt,
    TagName,
    Tag,
    TagQuoteD,
    TagQuoteS,
    Comment,
    RawBody,
    AttrName,
    AttrAfterEquals,
    DeclOrPi,
    Entity,
    EntityName,
    EntityNum,
    EntityDec,
    EntityHex,
}

/// What a strip found: the text length, the well-formed tags and the entities dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Stripped {
    pub length: usize,
    pub tags: usize,
    pub entities: usize,
}

/// The scan state carried across chunks of a growing buffer, so that stripping it chunk by chunk
/// gives what stripping it whole gives. Positions are absolute indices into that buffer.
#[derive(Debug)]
pub(super) struct Cursor {
    state: State,
    name_start: usize,
    raw_end: Option<&'static [u8]>,
    raw_match: usize,
    tags: usize,
    entities: usize,
    entity_start: usize,
    attr_name_start: usize,
    emit_attr_value: bool,
    written: usize,
    next: usize,
}

impl Cursor {
    pub(super) fn new() -> Self {
        Cursor {
            state: State::Text,
            name_start: 0,
            raw_end: None,
            raw_match: 0,
            tags: 0,
            entities: 0,
            entity_start: 0,
            attr_name_start: 0,
            emit_attr_value: false,
            written: 0,
            next: 0,
        }
    }

    pub(super) fn content_length(&self) -> usize {
        self.written
    }

    pub(super) fn tags(&self) -> usize {
        self.tags
    }
}

/// Strips tags and well-formed entities, for charset detection, where entities are ASCII that
/// only skews the byte statistics.
pub(super) fn strip_tags_and_entities(src: &[u8]) -> (Vec<u8>, Stripped) {
    strip(src, true)
}

/// Strips tags only, leaving entities for junk scoring to expand.
pub(super) fn strip_tags(src: &[u8]) -> (Vec<u8>, Stripped) {
    strip(src, false)
}

/// Strips `src[..end]` from where the cursor stopped, appending to `dst`; `last` when no more
/// input follows.
pub(super) fn strip_tags_resumable(
    src: &[u8],
    end: usize,
    dst: &mut Vec<u8>,
    cursor: &mut Cursor,
    last: bool,
) {
    scan(src, end, dst, cursor, false, last);
}

fn strip(src: &[u8], drop_entities: bool) -> (Vec<u8>, Stripped) {
    let mut cursor = Cursor::new();
    let mut dst = Vec::with_capacity(src.len());
    scan(src, src.len(), &mut dst, &mut cursor, drop_entities, true);
    if matches!(
        cursor.state,
        State::Entity | State::EntityName | State::EntityNum | State::EntityDec | State::EntityHex
    ) {
        dst.extend_from_slice(&src[cursor.entity_start..]);
    }
    let stripped = Stripped {
        length: dst.len(),
        tags: cursor.tags,
        entities: cursor.entities,
    };
    (dst, stripped)
}

fn scan(
    src: &[u8],
    end: usize,
    dst: &mut Vec<u8>,
    c: &mut Cursor,
    drop_entities: bool,
    last: bool,
) {
    let mut i = c.next;
    while i < end {
        let b = src[i];
        match c.state {
            State::Text => {
                if b == b'<' {
                    c.state = State::Lt;
                } else if b == b'&' && drop_entities {
                    c.state = State::Entity;
                    c.entity_start = i;
                } else {
                    dst.push(b);
                }
            }
            State::Lt => {
                if b == b'!' && i + 2 >= end && !last {
                    break;
                }
                if b == b'!' && i + 2 < end && src[i + 1] == b'-' && src[i + 2] == b'-' {
                    c.state = State::Comment;
                    c.tags += 1;
                    i += 2;
                } else if b == b'!' || b == b'?' {
                    c.state = State::DeclOrPi;
                    c.tags += 1;
                } else if b == b'/' || b.is_ascii_alphabetic() {
                    c.state = State::TagName;
                    c.name_start = i;
                    c.tags += 1;
                } else {
                    dst.push(b'<');
                    if b != b'<' {
                        dst.push(b);
                        c.state = State::Text;
                    }
                }
            }
            State::DeclOrPi => {
                if b == b'>' {
                    c.state = State::Text;
                }
            }
            State::Entity => {
                if b == b'#' {
                    c.state = State::EntityNum;
                } else if b.is_ascii_alphabetic() {
                    c.state = State::EntityName;
                } else {
                    dst.extend_from_slice(&src[c.entity_start..i]);
                    if b == b'<' {
                        c.state = State::Lt;
                    } else if b == b'&' {
                        c.entity_start = i;
                    } else {
                        dst.push(b);
                        c.state = State::Text;
                    }
                }
            }
            State::EntityName | State::EntityDec | State::EntityHex => {
                let continues = match c.state {
                    State::EntityName => b.is_ascii_alphabetic(),
                    State::EntityDec => b.is_ascii_digit(),
                    _ => b.is_ascii_hexdigit(),
                };
                if b == b';' {
                    c.entities += 1;
                    c.state = State::Text;
                } else if !(continues && i - c.entity_start <= MAX_ENTITY_BODY_LEN) {
                    leave_entity(src, i, b, dst, c);
                }
            }
            State::EntityNum => {
                if b == b'x' || b == b'X' {
                    c.state = State::EntityHex;
                } else if b.is_ascii_digit() {
                    c.state = State::EntityDec;
                } else {
                    leave_entity(src, i, b, dst, c);
                }
            }
            State::TagName => {
                if is_tag_name_terminator(b) {
                    let name = &src[c.name_start..i];
                    c.raw_end = if name.eq_ignore_ascii_case(SCRIPT) {
                        Some(END_SCRIPT)
                    } else if name.eq_ignore_ascii_case(STYLE) {
                        Some(END_STYLE)
                    } else {
                        None
                    };
                    c.state = if b == b'>' { after_tag(c) } else { State::Tag };
                }
            }
            State::Tag => {
                if b == b'"' {
                    c.state = State::TagQuoteD;
                } else if b == b'\'' {
                    c.state = State::TagQuoteS;
                } else if b == b'>' {
                    c.state = after_tag(c);
                } else if b.is_ascii_alphabetic() || b == b'-' || b == b'_' {
                    c.state = State::AttrName;
                    c.attr_name_start = i;
                }
            }
            State::AttrName => {
                if matches!(b, b'=' | b' ' | b'\t' | b'\n' | b'\r' | b'/' | b'>') {
                    let name = &src[c.attr_name_start..i];
                    c.emit_attr_value = TEXT_ATTRS
                        .iter()
                        .any(|attr| name.eq_ignore_ascii_case(attr));
                    if b == b'>' {
                        c.state = after_tag(c);
                        c.emit_attr_value = false;
                    } else if b == b'=' {
                        c.state = State::AttrAfterEquals;
                    } else {
                        c.emit_attr_value = false;
                        c.state = State::Tag;
                    }
                }
            }
            State::AttrAfterEquals => match b {
                b'"' | b'\'' => {
                    if c.emit_attr_value {
                        dst.push(b' ');
                    }
                    c.state = if b == b'"' {
                        State::TagQuoteD
                    } else {
                        State::TagQuoteS
                    };
                }
                b'>' => {
                    c.emit_attr_value = false;
                    c.state = after_tag(c);
                }
                b' ' | b'\t' | b'\n' | b'\r' => {}
                _ => {
                    c.emit_attr_value = false;
                    c.state = State::Tag;
                }
            },
            State::TagQuoteD | State::TagQuoteS => {
                let quote = if c.state == State::TagQuoteD {
                    b'"'
                } else {
                    b'\''
                };
                if b == quote {
                    c.emit_attr_value = false;
                    c.state = State::Tag;
                } else if c.emit_attr_value {
                    dst.push(b);
                }
            }
            State::Comment => {
                if b == b'>' && i >= 2 && src[i - 1] == b'-' && src[i - 2] == b'-' {
                    c.state = State::Text;
                }
            }
            State::RawBody => {
                let raw_end = c.raw_end.expect("a raw body has an end tag");
                if c.raw_match < raw_end.len() {
                    let target = raw_end[c.raw_match];
                    let actual = if target.is_ascii_lowercase() {
                        b | 0x20
                    } else {
                        b
                    };
                    if actual == target {
                        c.raw_match += 1;
                    } else {
                        c.raw_match = usize::from(b == b'<');
                    }
                } else if is_tag_name_terminator(b) {
                    c.raw_end = None;
                    c.raw_match = 0;
                    c.state = if b == b'>' { State::Text } else { State::Tag };
                } else {
                    c.raw_match = usize::from(b == b'<');
                }
            }
        }
        i += 1;
    }
    c.written = dst.len();
    c.next = i;
}

/// The state after a tag's `>`: a script or style body, or text.
fn after_tag(c: &mut Cursor) -> State {
    if c.raw_end.is_some() {
        c.raw_match = 0;
        State::RawBody
    } else {
        State::Text
    }
}

/// Gives up an entity that is not well-formed: its consumed bytes are text, and `b` is read
/// again as text.
fn leave_entity(src: &[u8], i: usize, b: u8, dst: &mut Vec<u8>, c: &mut Cursor) {
    dst.extend_from_slice(&src[c.entity_start..i]);
    if b == b'<' {
        c.state = State::Lt;
    } else if b == b'&' {
        c.entity_start = i;
        c.state = State::Entity;
    } else {
        dst.push(b);
        c.state = State::Text;
    }
}

fn is_tag_name_terminator(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/')
}
