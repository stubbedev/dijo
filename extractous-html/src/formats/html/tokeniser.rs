//! jsoup 1.23.2's `Tokeniser` and `TokeniserState` for HTML: the states of the WHATWG tokenizer
//! as jsoup implements them, its departures included. Among those: a NUL in data is kept, `\r`
//! is kept (no newline normalization), a U+FFFF in the text reads as the end of input, character
//! references follow jsoup's base and full entity sets, and an end tag in raw text may extend a
//! name the last start tag began. The XML-only states are left out.

use super::entities;
use super::reader::{equals_ignore_case, CharacterReader, EOF};
use super::token::{is_html_whitespace, Doctype, TagToken, Token, TokenData};

const NUL: u16 = 0;
const REPLACEMENT: u16 = 0xFFFD;
const TAB: u16 = 9;
const LF: u16 = 10;
const FF: u16 = 12;
const CR: u16 = 13;
const SPACE: u16 = 32;

const fn unit(c: u8) -> u16 {
    c as u16
}

const LT: u16 = unit(b'<');
const GT: u16 = unit(b'>');
const AMP: u16 = unit(b'&');
const SLASH: u16 = unit(b'/');
const BANG: u16 = unit(b'!');
const QUESTION: u16 = unit(b'?');
const DASH: u16 = unit(b'-');
const EQUALS: u16 = unit(b'=');
const QUOTE: u16 = unit(b'"');
const APOSTROPHE: u16 = unit(b'\'');
const BACKTICK: u16 = unit(b'`');

/// Characters some numeric references name instead of C1 controls (`win1252Extensions`).
const WIN1252_EXTENSIONS: [u32; 32] = [
    0x20AC, 0x0081, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039,
    0x0152, 0x008D, 0x017D, 0x008F, 0x0090, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014,
    0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x009D, 0x017E, 0x0178,
];

const ATTRIBUTE_NAME_ENDS: [u16; 12] = [
    TAB, LF, FF, CR, SPACE, QUOTE, APOSTROPHE, SLASH, LT, EQUALS, GT, QUESTION,
];
const UNQUOTED_VALUE_ENDS: [u16; 13] = [
    NUL, TAB, LF, FF, CR, SPACE, QUOTE, AMP, APOSTROPHE, LT, EQUALS, GT, BACKTICK,
];
const NOT_CHAR_REF: [u16; 7] = [TAB, LF, CR, FF, SPACE, LT, AMP];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum State {
    Data,
    CharacterReferenceInData,
    Rcdata,
    CharacterReferenceInRcdata,
    Rawtext,
    ScriptData,
    Plaintext,
    TagOpen,
    EndTagOpen,
    TagName,
    RcdataLessthanSign,
    RcdataEndTagOpen,
    RcdataEndTagName,
    RawtextLessthanSign,
    RawtextEndTagOpen,
    RawtextEndTagName,
    ScriptDataLessthanSign,
    ScriptDataEndTagOpen,
    ScriptDataEndTagName,
    ScriptDataEscapeStart,
    ScriptDataEscapeStartDash,
    ScriptDataEscaped,
    ScriptDataEscapedDash,
    ScriptDataEscapedDashDash,
    ScriptDataEscapedLessthanSign,
    ScriptDataEscapedEndTagOpen,
    ScriptDataEscapedEndTagName,
    ScriptDataDoubleEscapeStart,
    ScriptDataDoubleEscaped,
    ScriptDataDoubleEscapedDash,
    ScriptDataDoubleEscapedDashDash,
    ScriptDataDoubleEscapedLessthanSign,
    ScriptDataDoubleEscapeEnd,
    BeforeAttributeName,
    AttributeName,
    AfterAttributeName,
    BeforeAttributeValue,
    AttributeValueDoubleQuoted,
    AttributeValueSingleQuoted,
    AttributeValueUnquoted,
    AfterAttributeValueQuoted,
    SelfClosingStartTag,
    BogusComment,
    MarkupDeclarationOpen,
    CommentStart,
    CommentStartDash,
    Comment,
    CommentEndDash,
    CommentEnd,
    CommentEndBang,
    Doctype,
    BeforeDoctypeName,
    DoctypeName,
    AfterDoctypeName,
    AfterDoctypePublicKeyword,
    BeforeDoctypePublicIdentifier,
    DoctypePublicIdentifierDoubleQuoted,
    DoctypePublicIdentifierSingleQuoted,
    AfterDoctypePublicIdentifier,
    BetweenDoctypePublicAndSystemIdentifiers,
    AfterDoctypeSystemKeyword,
    BeforeDoctypeSystemIdentifier,
    DoctypeSystemIdentifierDoubleQuoted,
    DoctypeSystemIdentifierSingleQuoted,
    AfterDoctypeSystemIdentifier,
    BogusDoctype,
    CdataSection,
}

pub(super) struct Tokeniser<'a> {
    pub(super) reader: CharacterReader<'a>,
    state: State,
    emit_pending: Option<Token>,
    char_pending: TokenData,
    tag_pending: TagToken,
    doctype_pending: Doctype,
    comment_pending: TokenData,
    data_buffer: TokenData,
    last_start_tag: Option<Vec<u16>>,
}

impl<'a> Tokeniser<'a> {
    pub(super) fn new(reader: CharacterReader<'a>) -> Self {
        Tokeniser {
            reader,
            state: State::Data,
            emit_pending: None,
            char_pending: TokenData::default(),
            tag_pending: TagToken::new(true),
            doctype_pending: Doctype::default(),
            comment_pending: TokenData::default(),
            data_buffer: TokenData::default(),
            last_start_tag: None,
        }
    }

    /// The next token. `cdata_allowed` says whether the tree builder's current element is
    /// outside the HTML namespace, where `<![CDATA[` opens a CDATA section.
    pub(super) fn read(&mut self, cdata_allowed: bool) -> Token {
        while self.emit_pending.is_none() {
            self.step(cdata_allowed);
        }
        if self.char_pending.has_data() {
            return Token::Character {
                data: self.char_pending.take(),
                cdata: false,
            };
        }
        self.emit_pending
            .take()
            .unwrap_or_else(|| panic!("jsoup's tokeniser emitted nothing"))
    }

    /// Emits a token, as the tree builder does to close a self-closing element.
    pub(super) fn emit(&mut self, token: Token) {
        assert!(
            self.emit_pending.is_none(),
            "jsoup's tokeniser emitted a token over a pending one"
        );
        if let Token::Tag(tag) = &token {
            if tag.start {
                self.last_start_tag = Some(tag.name().to_vec());
            }
        }
        self.emit_pending = Some(token);
    }

    fn emit_units(&mut self, units: &[u16]) {
        self.char_pending.append(units);
    }

    fn emit_unit(&mut self, unit: u16) {
        self.char_pending.push(unit);
    }

    fn emit_str(&mut self, text: &str) {
        self.char_pending.push_str(text);
    }

    fn emit_code_points(&mut self, code_points: &[u32]) {
        for &code_point in code_points {
            self.char_pending.push_code_point(code_point);
        }
    }

    pub(super) fn transition(&mut self, state: State) {
        self.state = state;
    }

    fn advance_transition(&mut self, state: State) {
        self.transition(state);
        self.reader.advance();
    }

    fn create_tag_pending(&mut self, start: bool) {
        self.tag_pending = TagToken::new(start);
    }

    fn emit_tag_pending(&mut self) {
        self.tag_pending.finalise();
        let tag = std::mem::replace(&mut self.tag_pending, TagToken::new(true));
        self.emit(Token::Tag(tag));
    }

    fn create_comment_pending(&mut self) {
        self.comment_pending = TokenData::default();
    }

    fn emit_comment_pending(&mut self) {
        let data = self.comment_pending.take();
        self.emit(Token::Comment(data));
    }

    fn create_doctype_pending(&mut self) {
        self.doctype_pending = Doctype::default();
    }

    fn emit_doctype_pending(&mut self) {
        let doctype = std::mem::take(&mut self.doctype_pending);
        self.emit(Token::Doctype(doctype));
    }

    fn is_appropriate_end_tag_token(&self) -> bool {
        self.last_start_tag
            .as_deref()
            .is_some_and(|last| equals_ignore_case(self.tag_pending.name(), last))
    }

    fn is_appropriate_end_tag_prefix(&self, next: u16) -> bool {
        let Some(last) = self.last_start_tag.as_deref() else {
            return false;
        };
        let candidate: Vec<u16> = self.tag_pending.normal_name().encode_utf16().collect();
        candidate.len() < last.len()
            && equals_ignore_case(&last[..candidate.len()], &candidate)
            && last[candidate.len()] == next
    }

    /// A character reference after `&`, or `None` when there is none (`consumeCharacterReference`).
    fn consume_character_reference(
        &mut self,
        additional_allowed: Option<u16>,
        in_attribute: bool,
    ) -> Option<Vec<u32>> {
        let r = &mut self.reader;
        if r.is_empty() {
            return None;
        }
        if additional_allowed.is_some_and(|allowed| allowed == r.current()) {
            return None;
        }
        if r.matches_any(&NOT_CHAR_REF) {
            return None;
        }
        r.mark();
        if r.match_consume("#") {
            let hex = r.match_consume_ignore_case("X");
            let digits = if hex {
                r.consume_hex_sequence()
            } else {
                r.consume_digit_sequence()
            };
            if digits.is_empty() {
                r.rewind_to_mark();
                return None;
            }
            r.unmark();
            r.match_consume(";");
            let value = java_integer(digits, if hex { 16 } else { 10 });
            let code_point = match value {
                Some(value) if value <= 0x10FFFF => {
                    if (0x80..0xA0).contains(&value) {
                        WIN1252_EXTENSIONS[(value - 0x80) as usize]
                    } else {
                        value
                    }
                }
                _ => u32::from(REPLACEMENT),
            };
            return Some(vec![code_point]);
        }
        let mut name = String::from_utf16_lossy(r.consume_letter_then_digit_sequence());
        let looks_legit = r.matches(unit(b';'));
        let found = entities::is_base(&name) || (entities::is_named(&name) && looks_legit);
        if !found {
            r.rewind_to_mark();
            if in_attribute {
                return None;
            }
            let prefix = entities::find_prefix(&name);
            if prefix.is_empty() {
                return None;
            }
            r.match_consume(prefix);
            name = prefix.to_string();
        }
        if in_attribute
            && (r.matches_ascii_alpha()
                || r.matches_digit()
                || r.matches_any(&[EQUALS, DASH, unit(b'_')]))
        {
            r.rewind_to_mark();
            return None;
        }
        r.unmark();
        r.match_consume(";");
        let code_points = entities::codepoints(&name)
            .unwrap_or_else(|| panic!("jsoup has no code points for the entity {name}"));
        Some(code_points)
    }

    fn step(&mut self, cdata_allowed: bool) {
        match self.state {
            State::Data => match self.reader.current() {
                AMP => self.advance_transition(State::CharacterReferenceInData),
                LT => self.advance_transition(State::TagOpen),
                NUL => {
                    let c = self.reader.consume();
                    self.emit_unit(c);
                }
                EOF => self.emit(Token::Eof),
                _ => {
                    let data = self.reader.consume_data();
                    self.emit_units(data);
                }
            },
            State::CharacterReferenceInData => self.read_char_ref(State::Data),
            State::Rcdata => match self.reader.current() {
                AMP => self.advance_transition(State::CharacterReferenceInRcdata),
                LT => self.advance_transition(State::RcdataLessthanSign),
                NUL => {
                    self.reader.advance();
                    self.emit_unit(REPLACEMENT);
                }
                EOF => self.emit(Token::Eof),
                _ => {
                    let data = self.reader.consume_data();
                    self.emit_units(data);
                }
            },
            State::CharacterReferenceInRcdata => self.read_char_ref(State::Rcdata),
            State::Rawtext => self.read_raw_data(State::RawtextLessthanSign),
            State::ScriptData => self.read_raw_data(State::ScriptDataLessthanSign),
            State::Plaintext => match self.reader.current() {
                NUL => {
                    self.reader.advance();
                    self.emit_unit(REPLACEMENT);
                }
                EOF => self.emit(Token::Eof),
                _ => {
                    let data = self.reader.consume_to(NUL);
                    self.emit_units(data);
                }
            },
            State::TagOpen => match self.reader.current() {
                BANG => self.advance_transition(State::MarkupDeclarationOpen),
                SLASH => self.advance_transition(State::EndTagOpen),
                QUESTION => {
                    self.comment_pending = TokenData::default();
                    self.transition(State::BogusComment);
                }
                _ => {
                    if self.reader.matches_ascii_alpha() {
                        self.create_tag_pending(true);
                        self.transition(State::TagName);
                    } else {
                        self.emit_unit(LT);
                        self.transition(State::Data);
                    }
                }
            },
            State::EndTagOpen => {
                if self.reader.is_empty() {
                    self.emit_str("</");
                    self.transition(State::Data);
                } else if self.reader.matches_ascii_alpha() {
                    self.create_tag_pending(false);
                    self.transition(State::TagName);
                } else if self.reader.matches(GT) {
                    self.advance_transition(State::Data);
                } else {
                    self.comment_pending = TokenData::default();
                    self.comment_pending.push(SLASH);
                    self.transition(State::BogusComment);
                }
            }
            State::TagName => {
                let name = self.reader.consume_tag_name();
                self.tag_pending.append_tag_name(name);
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => self.transition(State::BeforeAttributeName),
                    SLASH => self.transition(State::SelfClosingStartTag),
                    GT => {
                        self.emit_tag_pending();
                        self.transition(State::Data);
                    }
                    NUL => self.tag_pending.append_tag_name(&[REPLACEMENT]),
                    EOF => self.transition(State::Data),
                    _ => self.tag_pending.append_tag_name(&[c]),
                }
            }
            State::RcdataLessthanSign => {
                if self.reader.matches(SLASH) {
                    self.advance_transition(State::RcdataEndTagOpen);
                } else {
                    self.emit_unit(LT);
                    self.transition(State::Rcdata);
                }
            }
            State::RcdataEndTagOpen => self.read_end_tag(State::RcdataEndTagName, State::Rcdata),
            State::RcdataEndTagName => self.handle_data_end_tag(State::Rcdata),
            State::RawtextLessthanSign => {
                if self.reader.matches(SLASH) {
                    self.advance_transition(State::RawtextEndTagOpen);
                } else {
                    self.emit_unit(LT);
                    self.transition(State::Rawtext);
                }
            }
            State::RawtextEndTagOpen => self.read_end_tag(State::RawtextEndTagName, State::Rawtext),
            State::RawtextEndTagName => self.handle_data_end_tag(State::Rawtext),
            State::ScriptDataLessthanSign => match self.reader.consume() {
                SLASH => self.transition(State::ScriptDataEndTagOpen),
                BANG => {
                    self.emit_str("<!");
                    self.transition(State::ScriptDataEscapeStart);
                }
                EOF => {
                    self.emit_unit(LT);
                    self.transition(State::Data);
                }
                _ => {
                    self.emit_unit(LT);
                    self.reader.unconsume();
                    self.transition(State::ScriptData);
                }
            },
            State::ScriptDataEndTagOpen => {
                self.read_end_tag(State::ScriptDataEndTagName, State::ScriptData)
            }
            State::ScriptDataEndTagName => self.handle_data_end_tag(State::ScriptData),
            State::ScriptDataEscapeStart => {
                if self.reader.matches(DASH) {
                    self.emit_unit(DASH);
                    self.advance_transition(State::ScriptDataEscapeStartDash);
                } else {
                    self.transition(State::ScriptData);
                }
            }
            State::ScriptDataEscapeStartDash => {
                if self.reader.matches(DASH) {
                    self.emit_unit(DASH);
                    self.advance_transition(State::ScriptDataEscapedDashDash);
                } else {
                    self.transition(State::ScriptData);
                }
            }
            State::ScriptDataEscaped => {
                if self.reader.is_empty() {
                    self.transition(State::Data);
                    return;
                }
                match self.reader.current() {
                    DASH => {
                        self.emit_unit(DASH);
                        self.advance_transition(State::ScriptDataEscapedDash);
                    }
                    LT => self.advance_transition(State::ScriptDataEscapedLessthanSign),
                    NUL => {
                        self.reader.advance();
                        self.emit_unit(REPLACEMENT);
                    }
                    _ => {
                        let data = self.reader.consume_to_any(&[DASH, LT, NUL]);
                        self.emit_units(data);
                    }
                }
            }
            State::ScriptDataEscapedDash => {
                if self.reader.is_empty() {
                    self.transition(State::Data);
                    return;
                }
                let c = self.reader.consume();
                match c {
                    DASH => {
                        self.emit_unit(c);
                        self.transition(State::ScriptDataEscapedDashDash);
                    }
                    LT => self.transition(State::ScriptDataEscapedLessthanSign),
                    NUL => {
                        self.emit_unit(REPLACEMENT);
                        self.transition(State::ScriptDataEscaped);
                    }
                    _ => {
                        self.emit_unit(c);
                        self.transition(State::ScriptDataEscaped);
                    }
                }
            }
            State::ScriptDataEscapedDashDash => {
                if self.reader.is_empty() {
                    self.transition(State::Data);
                    return;
                }
                let c = self.reader.consume();
                match c {
                    DASH => self.emit_unit(c),
                    LT => self.transition(State::ScriptDataEscapedLessthanSign),
                    GT => {
                        self.emit_unit(c);
                        self.transition(State::ScriptData);
                    }
                    NUL => {
                        self.emit_unit(REPLACEMENT);
                        self.transition(State::ScriptDataEscaped);
                    }
                    _ => {
                        self.emit_unit(c);
                        self.transition(State::ScriptDataEscaped);
                    }
                }
            }
            State::ScriptDataEscapedLessthanSign => {
                if self.reader.matches_ascii_alpha() {
                    self.data_buffer = TokenData::default();
                    let current = self.reader.current();
                    self.data_buffer.push(current);
                    self.emit_unit(LT);
                    self.emit_unit(current);
                    self.advance_transition(State::ScriptDataDoubleEscapeStart);
                } else if self.reader.matches(SLASH) {
                    self.advance_transition(State::ScriptDataEscapedEndTagOpen);
                } else {
                    self.emit_unit(LT);
                    self.transition(State::ScriptDataEscaped);
                }
            }
            State::ScriptDataEscapedEndTagOpen => {
                self.read_end_tag(State::ScriptDataEscapedEndTagName, State::ScriptDataEscaped)
            }
            State::ScriptDataEscapedEndTagName => {
                self.handle_data_end_tag(State::ScriptDataEscaped)
            }
            State::ScriptDataDoubleEscapeStart => self.handle_data_double_escape_tag(
                State::ScriptDataDoubleEscaped,
                State::ScriptDataEscaped,
            ),
            State::ScriptDataDoubleEscaped => {
                let c = self.reader.current();
                match c {
                    DASH => {
                        self.emit_unit(c);
                        self.advance_transition(State::ScriptDataDoubleEscapedDash);
                    }
                    LT => {
                        self.emit_unit(c);
                        self.advance_transition(State::ScriptDataDoubleEscapedLessthanSign);
                    }
                    NUL => {
                        self.reader.advance();
                        self.emit_unit(REPLACEMENT);
                    }
                    EOF => self.transition(State::Data),
                    _ => {
                        let data = self.reader.consume_to_any(&[DASH, LT, NUL]);
                        self.emit_units(data);
                    }
                }
            }
            State::ScriptDataDoubleEscapedDash => {
                let c = self.reader.consume();
                match c {
                    DASH => {
                        self.emit_unit(c);
                        self.transition(State::ScriptDataDoubleEscapedDashDash);
                    }
                    LT => {
                        self.emit_unit(c);
                        self.transition(State::ScriptDataDoubleEscapedLessthanSign);
                    }
                    NUL => {
                        self.emit_unit(REPLACEMENT);
                        self.transition(State::ScriptDataDoubleEscaped);
                    }
                    EOF => self.transition(State::Data),
                    _ => {
                        self.emit_unit(c);
                        self.transition(State::ScriptDataDoubleEscaped);
                    }
                }
            }
            State::ScriptDataDoubleEscapedDashDash => {
                let c = self.reader.consume();
                match c {
                    DASH => self.emit_unit(c),
                    LT => {
                        self.emit_unit(c);
                        self.transition(State::ScriptDataDoubleEscapedLessthanSign);
                    }
                    GT => {
                        self.emit_unit(c);
                        self.transition(State::ScriptData);
                    }
                    NUL => {
                        self.emit_unit(REPLACEMENT);
                        self.transition(State::ScriptDataDoubleEscaped);
                    }
                    EOF => self.transition(State::Data),
                    _ => {
                        self.emit_unit(c);
                        self.transition(State::ScriptDataDoubleEscaped);
                    }
                }
            }
            State::ScriptDataDoubleEscapedLessthanSign => {
                if self.reader.matches(SLASH) {
                    self.emit_unit(SLASH);
                    self.data_buffer = TokenData::default();
                    self.advance_transition(State::ScriptDataDoubleEscapeEnd);
                } else {
                    self.transition(State::ScriptDataDoubleEscaped);
                }
            }
            State::ScriptDataDoubleEscapeEnd => self.handle_data_double_escape_tag(
                State::ScriptDataEscaped,
                State::ScriptDataDoubleEscaped,
            ),
            State::BeforeAttributeName => {
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => {}
                    SLASH => self.transition(State::SelfClosingStartTag),
                    GT => {
                        self.emit_tag_pending();
                        self.transition(State::Data);
                    }
                    NUL => {
                        self.reader.unconsume();
                        self.tag_pending.new_attribute();
                        self.transition(State::AttributeName);
                    }
                    EOF => self.transition(State::Data),
                    QUOTE | APOSTROPHE | EQUALS => {
                        self.tag_pending.new_attribute();
                        self.tag_pending.append_attribute_name_unit(c);
                        self.transition(State::AttributeName);
                    }
                    _ => {
                        self.tag_pending.new_attribute();
                        self.reader.unconsume();
                        self.transition(State::AttributeName);
                    }
                }
            }
            State::AttributeName => {
                let name = self.reader.consume_to_any(&ATTRIBUTE_NAME_ENDS);
                self.tag_pending.append_attribute_name(name);
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => self.transition(State::AfterAttributeName),
                    SLASH => self.transition(State::SelfClosingStartTag),
                    EQUALS => self.transition(State::BeforeAttributeValue),
                    GT => {
                        self.emit_tag_pending();
                        self.transition(State::Data);
                    }
                    EOF => self.transition(State::Data),
                    _ => self.tag_pending.append_attribute_name_unit(c),
                }
            }
            State::AfterAttributeName => {
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => {}
                    SLASH => self.transition(State::SelfClosingStartTag),
                    EQUALS => self.transition(State::BeforeAttributeValue),
                    GT => {
                        self.emit_tag_pending();
                        self.transition(State::Data);
                    }
                    NUL => {
                        self.tag_pending.append_attribute_name_unit(REPLACEMENT);
                        self.transition(State::AttributeName);
                    }
                    EOF => self.transition(State::Data),
                    QUOTE | APOSTROPHE | LT => {
                        self.tag_pending.new_attribute();
                        self.tag_pending.append_attribute_name_unit(c);
                        self.transition(State::AttributeName);
                    }
                    _ => {
                        self.tag_pending.new_attribute();
                        self.reader.unconsume();
                        self.transition(State::AttributeName);
                    }
                }
            }
            State::BeforeAttributeValue => {
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => {}
                    QUOTE => self.transition(State::AttributeValueDoubleQuoted),
                    AMP => {
                        self.reader.unconsume();
                        self.transition(State::AttributeValueUnquoted);
                    }
                    APOSTROPHE => self.transition(State::AttributeValueSingleQuoted),
                    NUL => {
                        self.tag_pending.append_attribute_value_unit(REPLACEMENT);
                        self.transition(State::AttributeValueUnquoted);
                    }
                    EOF => {
                        self.emit_tag_pending();
                        self.transition(State::Data);
                    }
                    GT => {
                        self.emit_tag_pending();
                        self.transition(State::Data);
                    }
                    LT | EQUALS | BACKTICK => {
                        self.tag_pending.append_attribute_value_unit(c);
                        self.transition(State::AttributeValueUnquoted);
                    }
                    _ => {
                        self.reader.unconsume();
                        self.transition(State::AttributeValueUnquoted);
                    }
                }
            }
            State::AttributeValueDoubleQuoted => self.read_quoted_value(false),
            State::AttributeValueSingleQuoted => self.read_quoted_value(true),
            State::AttributeValueUnquoted => {
                let value = self.reader.consume_to_any(&UNQUOTED_VALUE_ENDS);
                if !value.is_empty() {
                    self.tag_pending.append_attribute_value(value);
                }
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => self.transition(State::BeforeAttributeName),
                    AMP => match self.consume_character_reference(Some(GT), true) {
                        Some(code_points) => self
                            .tag_pending
                            .append_attribute_value_code_points(&code_points),
                        None => self.tag_pending.append_attribute_value_unit(AMP),
                    },
                    GT => {
                        self.emit_tag_pending();
                        self.transition(State::Data);
                    }
                    NUL => self.tag_pending.append_attribute_value_unit(REPLACEMENT),
                    EOF => self.transition(State::Data),
                    _ => self.tag_pending.append_attribute_value_unit(c),
                }
            }
            State::AfterAttributeValueQuoted => {
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => self.transition(State::BeforeAttributeName),
                    SLASH => self.transition(State::SelfClosingStartTag),
                    GT => {
                        self.emit_tag_pending();
                        self.transition(State::Data);
                    }
                    EOF => self.transition(State::Data),
                    _ => {
                        self.reader.unconsume();
                        self.transition(State::BeforeAttributeName);
                    }
                }
            }
            State::SelfClosingStartTag => {
                let c = self.reader.consume();
                match c {
                    GT => {
                        self.tag_pending.self_closing = true;
                        self.emit_tag_pending();
                        self.transition(State::Data);
                    }
                    EOF => self.transition(State::Data),
                    _ => {
                        self.reader.unconsume();
                        self.transition(State::BeforeAttributeName);
                    }
                }
            }
            State::BogusComment => {
                let text = self.reader.consume_to(GT);
                self.comment_pending.append(text);
                let next = self.reader.current();
                if next == GT || next == EOF {
                    self.reader.consume();
                    self.emit_comment_pending();
                    self.transition(State::Data);
                }
            }
            State::MarkupDeclarationOpen => {
                if self.reader.match_consume("--") {
                    self.create_comment_pending();
                    self.transition(State::CommentStart);
                } else if self.reader.match_consume_ignore_case("DOCTYPE") {
                    self.transition(State::Doctype);
                } else if self.reader.match_consume("[CDATA[") {
                    if cdata_allowed {
                        self.data_buffer = TokenData::default();
                        self.transition(State::CdataSection);
                    } else {
                        self.comment_pending = TokenData::default();
                        self.comment_pending.push_str("[CDATA[");
                        self.transition(State::BogusComment);
                    }
                } else {
                    self.comment_pending = TokenData::default();
                    self.transition(State::BogusComment);
                }
            }
            State::CommentStart => {
                let c = self.reader.consume();
                match c {
                    DASH => self.transition(State::CommentStartDash),
                    NUL => {
                        self.comment_pending.push(REPLACEMENT);
                        self.transition(State::Comment);
                    }
                    GT | EOF => {
                        self.emit_comment_pending();
                        self.transition(State::Data);
                    }
                    _ => {
                        self.reader.unconsume();
                        self.transition(State::Comment);
                    }
                }
            }
            State::CommentStartDash => {
                let c = self.reader.consume();
                match c {
                    DASH => self.transition(State::CommentEnd),
                    NUL => {
                        self.comment_pending.push(REPLACEMENT);
                        self.transition(State::Comment);
                    }
                    GT | EOF => {
                        self.emit_comment_pending();
                        self.transition(State::Data);
                    }
                    _ => {
                        self.comment_pending.push(c);
                        self.transition(State::Comment);
                    }
                }
            }
            State::Comment => match self.reader.current() {
                DASH => self.advance_transition(State::CommentEndDash),
                NUL => {
                    self.reader.advance();
                    self.comment_pending.push(REPLACEMENT);
                }
                EOF => {
                    self.emit_comment_pending();
                    self.transition(State::Data);
                }
                _ => {
                    let text = self.reader.consume_to_any(&[DASH, NUL]);
                    self.comment_pending.append(text);
                }
            },
            State::CommentEndDash => {
                let c = self.reader.consume();
                match c {
                    DASH => self.transition(State::CommentEnd),
                    NUL => {
                        self.comment_pending.append(&[DASH, REPLACEMENT]);
                        self.transition(State::Comment);
                    }
                    EOF => {
                        self.emit_comment_pending();
                        self.transition(State::Data);
                    }
                    _ => {
                        self.comment_pending.append(&[DASH, c]);
                        self.transition(State::Comment);
                    }
                }
            }
            State::CommentEnd => {
                let c = self.reader.consume();
                match c {
                    GT => {
                        self.emit_comment_pending();
                        self.transition(State::Data);
                    }
                    NUL => {
                        self.comment_pending.append(&[DASH, DASH, REPLACEMENT]);
                        self.transition(State::Comment);
                    }
                    BANG => self.transition(State::CommentEndBang),
                    DASH => self.comment_pending.push(DASH),
                    EOF => {
                        self.emit_comment_pending();
                        self.transition(State::Data);
                    }
                    _ => {
                        self.comment_pending.append(&[DASH, DASH, c]);
                        self.transition(State::Comment);
                    }
                }
            }
            State::CommentEndBang => {
                let c = self.reader.consume();
                match c {
                    DASH => {
                        self.comment_pending.append(&[DASH, DASH, BANG]);
                        self.transition(State::CommentEndDash);
                    }
                    GT => {
                        self.emit_comment_pending();
                        self.transition(State::Data);
                    }
                    NUL => {
                        self.comment_pending
                            .append(&[DASH, DASH, BANG, REPLACEMENT]);
                        self.transition(State::Comment);
                    }
                    EOF => {
                        self.emit_comment_pending();
                        self.transition(State::Data);
                    }
                    _ => {
                        self.comment_pending.append(&[DASH, DASH, BANG, c]);
                        self.transition(State::Comment);
                    }
                }
            }
            State::Doctype => {
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => self.transition(State::BeforeDoctypeName),
                    EOF | GT => {
                        self.create_doctype_pending();
                        self.doctype_pending.force_quirks = true;
                        self.emit_doctype_pending();
                        self.transition(State::Data);
                    }
                    _ => self.transition(State::BeforeDoctypeName),
                }
            }
            State::BeforeDoctypeName => {
                if self.reader.matches_ascii_alpha() {
                    self.create_doctype_pending();
                    self.transition(State::DoctypeName);
                    return;
                }
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => {}
                    NUL => {
                        self.create_doctype_pending();
                        self.doctype_pending.name.push(REPLACEMENT);
                        self.transition(State::DoctypeName);
                    }
                    EOF => {
                        self.create_doctype_pending();
                        self.doctype_pending.force_quirks = true;
                        self.emit_doctype_pending();
                        self.transition(State::Data);
                    }
                    _ => {
                        self.create_doctype_pending();
                        self.doctype_pending.name.push(c);
                        self.transition(State::DoctypeName);
                    }
                }
            }
            State::DoctypeName => {
                if self.reader.matches_ascii_alpha() {
                    let name = self.reader.consume_letter_sequence();
                    self.doctype_pending.name.append(name);
                    return;
                }
                let c = self.reader.consume();
                match c {
                    GT => {
                        self.emit_doctype_pending();
                        self.transition(State::Data);
                    }
                    TAB | LF | CR | FF | SPACE => self.transition(State::AfterDoctypeName),
                    NUL => self.doctype_pending.name.push(REPLACEMENT),
                    EOF => {
                        self.doctype_pending.force_quirks = true;
                        self.emit_doctype_pending();
                        self.transition(State::Data);
                    }
                    _ => self.doctype_pending.name.push(c),
                }
            }
            State::AfterDoctypeName => {
                if self.reader.is_empty() {
                    self.doctype_pending.force_quirks = true;
                    self.emit_doctype_pending();
                    self.transition(State::Data);
                    return;
                }
                if self.reader.matches_any(&[TAB, LF, CR, FF, SPACE]) {
                    self.reader.advance();
                } else if self.reader.matches(GT) {
                    self.emit_doctype_pending();
                    self.advance_transition(State::Data);
                } else if self.reader.match_consume_ignore_case("PUBLIC") {
                    self.doctype_pending.pub_sys_key = Some("PUBLIC");
                    self.transition(State::AfterDoctypePublicKeyword);
                } else if self.reader.match_consume_ignore_case("SYSTEM") {
                    self.doctype_pending.pub_sys_key = Some("SYSTEM");
                    self.transition(State::AfterDoctypeSystemKeyword);
                } else {
                    self.doctype_pending.force_quirks = true;
                    self.advance_transition(State::BogusDoctype);
                }
            }
            State::AfterDoctypePublicKeyword => {
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => {
                        self.transition(State::BeforeDoctypePublicIdentifier)
                    }
                    QUOTE => self.transition(State::DoctypePublicIdentifierDoubleQuoted),
                    APOSTROPHE => self.transition(State::DoctypePublicIdentifierSingleQuoted),
                    GT | EOF => self.quirky_doctype_end(),
                    _ => {
                        self.doctype_pending.force_quirks = true;
                        self.transition(State::BogusDoctype);
                    }
                }
            }
            State::BeforeDoctypePublicIdentifier => {
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => {}
                    QUOTE => self.transition(State::DoctypePublicIdentifierDoubleQuoted),
                    APOSTROPHE => self.transition(State::DoctypePublicIdentifierSingleQuoted),
                    GT | EOF => self.quirky_doctype_end(),
                    _ => {
                        self.doctype_pending.force_quirks = true;
                        self.transition(State::BogusDoctype);
                    }
                }
            }
            State::DoctypePublicIdentifierDoubleQuoted => {
                self.read_doctype_identifier(QUOTE, true, State::AfterDoctypePublicIdentifier)
            }
            State::DoctypePublicIdentifierSingleQuoted => {
                self.read_doctype_identifier(APOSTROPHE, true, State::AfterDoctypePublicIdentifier)
            }
            State::AfterDoctypePublicIdentifier => {
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => {
                        self.transition(State::BetweenDoctypePublicAndSystemIdentifiers)
                    }
                    GT => {
                        self.emit_doctype_pending();
                        self.transition(State::Data);
                    }
                    QUOTE => self.transition(State::DoctypeSystemIdentifierDoubleQuoted),
                    APOSTROPHE => self.transition(State::DoctypeSystemIdentifierSingleQuoted),
                    EOF => self.quirky_doctype_end(),
                    _ => {
                        self.doctype_pending.force_quirks = true;
                        self.transition(State::BogusDoctype);
                    }
                }
            }
            State::BetweenDoctypePublicAndSystemIdentifiers => {
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => {}
                    GT => {
                        self.emit_doctype_pending();
                        self.transition(State::Data);
                    }
                    QUOTE => self.transition(State::DoctypeSystemIdentifierDoubleQuoted),
                    APOSTROPHE => self.transition(State::DoctypeSystemIdentifierSingleQuoted),
                    EOF => self.quirky_doctype_end(),
                    _ => {
                        self.doctype_pending.force_quirks = true;
                        self.transition(State::BogusDoctype);
                    }
                }
            }
            State::AfterDoctypeSystemKeyword => {
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => {
                        self.transition(State::BeforeDoctypeSystemIdentifier)
                    }
                    GT | EOF => self.quirky_doctype_end(),
                    QUOTE => self.transition(State::DoctypeSystemIdentifierDoubleQuoted),
                    APOSTROPHE => self.transition(State::DoctypeSystemIdentifierSingleQuoted),
                    _ => {
                        self.doctype_pending.force_quirks = true;
                        self.emit_doctype_pending();
                    }
                }
            }
            State::BeforeDoctypeSystemIdentifier => {
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => {}
                    QUOTE => self.transition(State::DoctypeSystemIdentifierDoubleQuoted),
                    APOSTROPHE => self.transition(State::DoctypeSystemIdentifierSingleQuoted),
                    GT | EOF => self.quirky_doctype_end(),
                    _ => {
                        self.doctype_pending.force_quirks = true;
                        self.transition(State::BogusDoctype);
                    }
                }
            }
            State::DoctypeSystemIdentifierDoubleQuoted => {
                self.read_doctype_identifier(QUOTE, false, State::AfterDoctypeSystemIdentifier)
            }
            State::DoctypeSystemIdentifierSingleQuoted => {
                self.read_doctype_identifier(APOSTROPHE, false, State::AfterDoctypeSystemIdentifier)
            }
            State::AfterDoctypeSystemIdentifier => {
                let c = self.reader.consume();
                match c {
                    TAB | LF | CR | FF | SPACE => {}
                    GT => {
                        self.emit_doctype_pending();
                        self.transition(State::Data);
                    }
                    EOF => self.quirky_doctype_end(),
                    _ => self.transition(State::BogusDoctype),
                }
            }
            State::BogusDoctype => {
                let c = self.reader.consume();
                if c == GT || c == EOF {
                    self.emit_doctype_pending();
                    self.transition(State::Data);
                }
            }
            State::CdataSection => {
                let data = self.reader.consume_to_seq(&[unit(b']'), unit(b']'), GT]);
                self.data_buffer.append(data);
                if self.reader.match_consume("]]>") || self.reader.is_empty() {
                    let data = self.data_buffer.take();
                    self.emit(Token::Character { data, cdata: true });
                    self.transition(State::Data);
                }
            }
        }
    }

    /// Emits the doctype pending, forcing quirks mode, and returns to data.
    fn quirky_doctype_end(&mut self) {
        self.doctype_pending.force_quirks = true;
        self.emit_doctype_pending();
        self.transition(State::Data);
    }

    fn read_doctype_identifier(&mut self, quote: u16, public: bool, after: State) {
        let c = self.reader.consume();
        let identifier = if public {
            &mut self.doctype_pending.public_identifier
        } else {
            &mut self.doctype_pending.system_identifier
        };
        match c {
            _ if c == quote => self.transition(after),
            NUL => identifier.push(REPLACEMENT),
            GT | EOF => self.quirky_doctype_end(),
            _ => identifier.push(c),
        }
    }

    fn read_quoted_value(&mut self, single: bool) {
        let value = self.reader.consume_attribute_quoted(single);
        if value.is_empty() {
            self.tag_pending.set_empty_attribute_value();
        } else {
            self.tag_pending.append_attribute_value(value);
        }
        let quote = if single { APOSTROPHE } else { QUOTE };
        let c = self.reader.consume();
        match c {
            _ if c == quote => self.transition(State::AfterAttributeValueQuoted),
            AMP => match self.consume_character_reference(Some(quote), true) {
                Some(code_points) => self
                    .tag_pending
                    .append_attribute_value_code_points(&code_points),
                None => self.tag_pending.append_attribute_value_unit(AMP),
            },
            NUL => self.tag_pending.append_attribute_value_unit(REPLACEMENT),
            EOF => self.transition(State::Data),
            _ => self.tag_pending.append_attribute_value_unit(c),
        }
    }

    fn read_char_ref(&mut self, advance: State) {
        match self.consume_character_reference(None, false) {
            None => self.emit_unit(AMP),
            Some(code_points) => self.emit_code_points(&code_points),
        }
        self.transition(advance);
    }

    fn read_raw_data(&mut self, advance: State) {
        match self.reader.current() {
            LT => self.advance_transition(advance),
            NUL => {
                self.reader.advance();
                self.emit_unit(REPLACEMENT);
            }
            EOF => self.emit(Token::Eof),
            _ => {
                let data = self.reader.consume_raw_data();
                self.emit_units(data);
            }
        }
    }

    fn read_end_tag(&mut self, end_tag_name: State, text: State) {
        if self.reader.matches_ascii_alpha() {
            self.create_tag_pending(false);
            self.transition(end_tag_name);
        } else {
            self.emit_str("</");
            self.transition(text);
        }
    }

    fn handle_data_end_tag(&mut self, else_transition: State) {
        if self.reader.matches_ascii_alpha() {
            let name = self.reader.consume_letter_sequence();
            self.tag_pending.append_tag_name(name);
            return;
        }
        if !self.reader.is_empty() {
            let c = self.reader.current();
            if is_custom_end_tag_name_char(c) && self.is_appropriate_end_tag_prefix(c) {
                self.reader.advance();
                self.tag_pending.append_tag_name(&[c]);
                return;
            }
        }
        if self.is_appropriate_end_tag_token() && !self.reader.is_empty() {
            match self.reader.current() {
                TAB | LF | CR | FF | SPACE => {
                    self.reader.advance();
                    self.transition(State::BeforeAttributeName);
                    return;
                }
                SLASH => {
                    self.reader.advance();
                    self.transition(State::SelfClosingStartTag);
                    return;
                }
                GT => {
                    self.reader.advance();
                    self.emit_tag_pending();
                    self.transition(State::Data);
                    return;
                }
                _ => {}
            }
        }
        self.emit_str("</");
        let name = self.tag_pending.name().to_vec();
        self.emit_units(&name);
        self.transition(else_transition);
    }

    fn handle_data_double_escape_tag(&mut self, primary: State, fallback: State) {
        if self.reader.matches_ascii_alpha() {
            let name = self.reader.consume_letter_sequence();
            self.data_buffer.append(name);
            self.emit_units(name);
            return;
        }
        let c = self.reader.consume();
        match c {
            TAB | LF | CR | FF | SPACE | SLASH | GT => {
                let script: Vec<u16> = "script".encode_utf16().collect();
                if self.data_buffer.value() == script.as_slice() {
                    self.transition(primary);
                } else {
                    self.transition(fallback);
                }
                self.emit_unit(c);
            }
            _ => {
                self.reader.unconsume();
                self.transition(fallback);
            }
        }
    }
}

fn is_custom_end_tag_name_char(c: u16) -> bool {
    c != LT && c != SLASH && c != GT && !is_html_whitespace(c)
}

/// Java's `Integer.valueOf(digits, radix)`, `None` when it overflows an `int`.
fn java_integer(digits: &[u16], radix: u32) -> Option<u32> {
    let mut value: u64 = 0;
    for &d in digits {
        let digit = char::from_u32(u32::from(d))
            .and_then(|c| c.to_digit(radix))
            .unwrap_or_else(|| panic!("jsoup read a non-digit into a character reference"));
        value = value * u64::from(radix) + u64::from(digit);
        if value > i32::MAX as u64 {
            return None;
        }
    }
    Some(value as u32)
}

#[cfg(test)]
mod tests {
    use super::super::reader::Source;
    use super::*;

    fn tokens(text: &str) -> Vec<String> {
        let units: Vec<u16> = text.encode_utf16().collect();
        let mut tokeniser = Tokeniser::new(CharacterReader::new(&units, Source::String));
        let mut out = Vec::new();
        loop {
            match tokeniser.read(false) {
                Token::Eof => return out,
                Token::Character { data, .. } => {
                    out.push(format!("C:{}", String::from_utf16_lossy(&data)))
                }
                Token::Tag(tag) => out.push(format!(
                    "{}:{}{}",
                    if tag.start { "S" } else { "E" },
                    tag.normal_name(),
                    tag.attributes
                        .iter()
                        .flatten()
                        .map(|a| format!(
                            " {}={:?}",
                            String::from_utf16_lossy(&a.name),
                            a.value.as_deref().map(String::from_utf16_lossy)
                        ))
                        .collect::<String>()
                )),
                Token::Comment(data) => out.push(format!("!:{}", String::from_utf16_lossy(&data))),
                Token::Doctype(d) => {
                    out.push(format!("D:{}", String::from_utf16_lossy(d.name.value())))
                }
            }
        }
    }

    #[test]
    fn tokenises_as_jsoup_does() {
        assert_eq!(
            tokens("<!doctype html><P Class=a ID='b' x>t&amp;&notit; &#x80;</p><!--c-->"),
            [
                "D:html",
                "S:p Class=Some(\"a\") ID=Some(\"b\") x=None",
                "C:t&¬it; €",
                "E:p",
                "!:c"
            ]
        );
        assert_eq!(tokens("a\r\nb\0c"), ["C:a\r\nb\0c"]);
        assert_eq!(tokens("before\u{FFFF}after"), ["C:before\u{FFFF}after"]);
        assert_eq!(tokens("a<b>\u{FFFF}c"), ["C:a", "S:b"]);
        assert_eq!(
            tokens("<a href=\"x&copy=1\">"),
            ["S:a href=Some(\"x&copy=1\")"]
        );
    }
}
