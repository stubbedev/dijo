//! jsoup's tokens (`Token`, `TokenData`), with text held in UTF-16 units as Java holds it.

/// Text a token builds up, which may be unset or set and empty (`TokenData`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct TokenData(Option<Vec<u16>>);

impl TokenData {
    pub(super) fn has_data(&self) -> bool {
        self.0.is_some()
    }

    pub(super) fn value(&self) -> &[u16] {
        self.0.as_deref().unwrap_or(&[])
    }

    pub(super) fn set(&mut self, units: &[u16]) {
        self.0 = Some(units.to_vec());
    }

    pub(super) fn append(&mut self, units: &[u16]) {
        self.0.get_or_insert_with(Vec::new).extend_from_slice(units);
    }

    pub(super) fn push(&mut self, unit: u16) {
        self.0.get_or_insert_with(Vec::new).push(unit);
    }

    pub(super) fn push_str(&mut self, text: &str) {
        self.0
            .get_or_insert_with(Vec::new)
            .extend(text.encode_utf16());
    }

    /// Java's `appendCodePoint`: a code point below U+10000, a surrogate among them, as one unit.
    pub(super) fn push_code_point(&mut self, code_point: u32) {
        push_code_point(self.0.get_or_insert_with(Vec::new), code_point);
    }

    pub(super) fn take(&mut self) -> Vec<u16> {
        self.0.take().unwrap_or_default()
    }
}

pub(super) fn push_code_point(units: &mut Vec<u16>, code_point: u32) {
    if code_point < 0x10000 {
        units.push(code_point as u16);
    } else {
        let offset = code_point - 0x10000;
        units.push(0xD800 + (offset >> 10) as u16);
        units.push(0xDC00 + (offset & 0x3FF) as u16);
    }
}

/// Java's `String.trim` over UTF-16 units.
pub(super) fn java_trim(units: &[u16]) -> &[u16] {
    let start = units.iter().position(|&u| u > 0x20).unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|&u| u > 0x20)
        .map_or(start, |i| i + 1);
    &units[start..end]
}

/// UTF-16 units as a string: a lone surrogate, which no Rust string holds, becomes U+FFFD, as
/// Tika writes it in text, attributes and metadata alike.
pub(super) fn string(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}

/// An attribute as the tokeniser reads it; `None` for a value-less (boolean) attribute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Attribute {
    pub(super) name: Vec<u16>,
    pub(super) value: Option<Vec<u16>>,
}

/// The most attributes a tag keeps (`Token.Tag.MaxAttributes`).
const MAX_ATTRIBUTES: usize = 512;

/// A start or end tag (`Token.Tag`).
#[derive(Debug, Default, Clone)]
pub(super) struct TagToken {
    pub(super) start: bool,
    name: TokenData,
    normal_name: Option<String>,
    pub(super) self_closing: bool,
    /// `None` until the first attribute starts, as jsoup's `attributes` stay null.
    pub(super) attributes: Option<Vec<Attribute>>,
    attr_name: TokenData,
    attr_value: TokenData,
    has_empty_attr_value: bool,
}

impl TagToken {
    pub(super) fn new(start: bool) -> Self {
        TagToken {
            start,
            ..Default::default()
        }
    }

    /// A tag the tree builder makes up, named in lower case already.
    pub(super) fn named(start: bool, name: &str) -> Self {
        let mut tag = TagToken::new(start);
        tag.set_name(name);
        tag
    }

    pub(super) fn name(&self) -> &[u16] {
        self.name.value()
    }

    pub(super) fn set_name(&mut self, name: &str) {
        self.name.set(&name.encode_utf16().collect::<Vec<_>>());
        self.update_normal_name();
    }

    /// The lower case name the tree builder decides by (`normalName`).
    pub(super) fn normal_name(&self) -> &str {
        self.normal_name.as_deref().unwrap_or("")
    }

    fn update_normal_name(&mut self) {
        self.normal_name = Some(string(self.name.value()).to_lowercase());
    }

    pub(super) fn append_tag_name(&mut self, units: &[u16]) {
        let replaced: Vec<u16> = units
            .iter()
            .map(|&u| if u == 0 { 0xFFFD } else { u })
            .collect();
        self.name.append(&replaced);
        self.update_normal_name();
    }

    pub(super) fn append_attribute_name(&mut self, units: &[u16]) {
        let replaced: Vec<u16> = units
            .iter()
            .map(|&u| if u == 0 { 0xFFFD } else { u })
            .collect();
        self.attr_name.append(&replaced);
    }

    pub(super) fn append_attribute_name_unit(&mut self, unit: u16) {
        self.attr_name.push(unit);
    }

    pub(super) fn append_attribute_value(&mut self, units: &[u16]) {
        self.attr_value.append(units);
    }

    pub(super) fn append_attribute_value_unit(&mut self, unit: u16) {
        self.attr_value.push(unit);
    }

    pub(super) fn append_attribute_value_code_points(&mut self, code_points: &[u32]) {
        for &code_point in code_points {
            self.attr_value.push_code_point(code_point);
        }
    }

    pub(super) fn set_empty_attribute_value(&mut self) {
        self.has_empty_attr_value = true;
    }

    pub(super) fn new_attribute(&mut self) {
        let attributes = self.attributes.get_or_insert_with(Vec::new);
        if self.attr_name.has_data() && attributes.len() < MAX_ATTRIBUTES {
            let name = java_trim(self.attr_name.value());
            if !name.is_empty() {
                let value = if self.attr_value.has_data() {
                    Some(self.attr_value.value().to_vec())
                } else if self.has_empty_attr_value {
                    Some(Vec::new())
                } else {
                    None
                };
                attributes.push(Attribute {
                    name: name.to_vec(),
                    value,
                });
            }
        }
        self.attr_name = TokenData::default();
        self.attr_value = TokenData::default();
        self.has_empty_attr_value = false;
    }

    pub(super) fn finalise(&mut self) {
        if self.attr_name.has_data() {
            self.new_attribute();
        }
    }

    pub(super) fn has_attribute_ignore_case(&self, key: &str) -> bool {
        let key: Vec<u16> = key.encode_utf16().collect();
        self.attributes.as_ref().is_some_and(|attributes| {
            attributes
                .iter()
                .any(|attribute| super::reader::equals_ignore_case(&attribute.name, &key))
        })
    }
}

/// A doctype (`Token.Doctype`).
#[derive(Debug, Default, Clone)]
pub(super) struct Doctype {
    pub(super) name: TokenData,
    pub(super) pub_sys_key: Option<&'static str>,
    pub(super) public_identifier: TokenData,
    pub(super) system_identifier: TokenData,
    pub(super) force_quirks: bool,
}

#[derive(Debug, Clone)]
pub(super) enum Token {
    Doctype(Doctype),
    Tag(TagToken),
    Comment(Vec<u16>),
    Character { data: Vec<u16>, cdata: bool },
    Eof,
}

impl Token {
    pub(super) fn is_start_tag(&self) -> bool {
        matches!(self, Token::Tag(tag) if tag.start)
    }

    /// Whether this is a start tag of normal name `name`.
    pub(super) fn is_start_named(&self, name: &str) -> bool {
        matches!(self, Token::Tag(tag) if tag.start && tag.normal_name() == name)
    }

    /// Whether this is an end tag of normal name `name`.
    pub(super) fn is_end_named(&self, name: &str) -> bool {
        matches!(self, Token::Tag(tag) if !tag.start && tag.normal_name() == name)
    }
}

/// jsoup's `StringUtil.isWhitespace` for the tokens' text: space, tab, line feed, form feed or
/// carriage return.
pub(super) fn is_html_whitespace(unit: u16) -> bool {
    matches!(unit, 0x20 | 0x09 | 0x0A | 0x0C | 0x0D)
}

/// jsoup's `StringUtil.isBlank`.
pub(super) fn is_blank(units: &[u16]) -> bool {
    units.iter().all(|&u| is_html_whitespace(u))
}
