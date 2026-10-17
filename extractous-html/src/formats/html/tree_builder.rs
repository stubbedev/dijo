//! jsoup 1.23.2's `HtmlTreeBuilder` and `HtmlTreeBuilderState`: the insertion modes of the
//! WHATWG tree builder as jsoup implements them, with its departures kept. Among those: whitespace
//! before `head` and after `body` stays in the tree, text never goes to a foster parent, `noscript`
//! holds its content as an island of markup, the stack is at most 512 elements deep, the active
//! formatting elements are reconstructed at most 12 back, and the tags Tika lets close themselves
//! close when written `<tag/>`.

use super::dom::{Attribute, Dom, NodeData, NodeId, DOCUMENT};
use super::reader::{equals_ignore_case, CharacterReader, Source};
use super::tags::{
    Namespace, TagSet, BUTTON_SCOPE, DATA, IMPLIED_END, LIST_SCOPE, RCDATA, SCOPE,
    SELECT_SCOPE_MEMBER, SPECIAL, TABLE_SCOPE, THOROUGH_IMPLIED_END,
};
use super::token::{is_blank, string, TagToken, Token};
use super::tokeniser::{State, Tokeniser};

const MAX_DEPTH: usize = 512;
const MAX_USED_FORMATTING_ELEMENTS: usize = 12;
const MAX_STACK_SCAN: usize = 24;

const IN_HEAD_EMPTY: &[&str] = &["base", "basefont", "bgsound", "command", "link"];
const IN_HEAD_RAW: &[&str] = &["noframes", "style"];
const IN_HEAD_END: &[&str] = &["body", "br", "html"];
const AFTER_HEAD_BODY: &[&str] = &["body", "br", "html"];
const BEFORE_HTML_TO_HEAD: &[&str] = &["body", "br", "head", "html"];
const IN_BODY_START_TO_HEAD: &[&str] = &[
    "base", "basefont", "bgsound", "command", "link", "meta", "noframes", "script", "style",
    "template", "title",
];
const IN_BODY_START_P_CLOSERS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dir",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "header",
    "hgroup",
    "menu",
    "nav",
    "ol",
    "p",
    "section",
    "summary",
    "ul",
];
const HEADINGS: &[&str] = &["h1", "h2", "h3", "h4", "h5", "h6"];
const IN_BODY_START_LI_BREAKERS: &[&str] = &["address", "div", "p"];
const DD_DT: &[&str] = &["dd", "dt"];
const IN_BODY_START_APPLETS: &[&str] = &["applet", "marquee", "object"];
const IN_BODY_START_MEDIA: &[&str] = &["param", "source", "track"];
const IN_BODY_START_DROP: &[&str] = &[
    "caption", "col", "colgroup", "frame", "head", "tbody", "td", "tfoot", "th", "thead", "tr",
];
const IN_BODY_END_CLOSERS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "button",
    "center",
    "details",
    "dir",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "header",
    "hgroup",
    "listing",
    "menu",
    "nav",
    "ol",
    "pre",
    "section",
    "summary",
    "ul",
];
const IN_BODY_END_ADOPTION_FORMATTERS: &[&str] = &[
    "a", "b", "big", "code", "em", "font", "i", "nobr", "s", "small", "strike", "strong", "tt", "u",
];
const IN_TABLE_TO_BODY: &[&str] = &["tbody", "tfoot", "thead"];
const IN_TABLE_ADD_BODY: &[&str] = &["td", "th", "tr"];
const IN_TABLE_TO_HEAD: &[&str] = &["script", "style", "template"];
const IN_CELL_NAMES: &[&str] = &["td", "th"];
const IN_CELL_BODY: &[&str] = &["body", "caption", "col", "colgroup", "html"];
const IN_CELL_TABLE: &[&str] = &["table", "tbody", "tfoot", "thead", "tr"];
const IN_CELL_COL: &[&str] = &[
    "caption", "col", "colgroup", "tbody", "td", "tfoot", "th", "thead", "tr",
];
const IN_TABLE_END_ERR: &[&str] = &[
    "body", "caption", "col", "colgroup", "html", "tbody", "td", "tfoot", "th", "thead", "tr",
];
const IN_TABLE_FOSTER: &[&str] = &["table", "tbody", "tfoot", "thead", "tr"];
const IN_TABLE_BODY_EXIT: &[&str] = &["caption", "col", "colgroup", "tbody", "tfoot", "thead"];
const IN_TABLE_BODY_END_IGNORE: &[&str] = &[
    "body", "caption", "col", "colgroup", "html", "td", "th", "tr",
];
const IN_ROW_MISSING: &[&str] = &[
    "caption", "col", "colgroup", "tbody", "tfoot", "thead", "tr",
];
const IN_ROW_IGNORE: &[&str] = &["body", "caption", "col", "colgroup", "html", "td", "th"];
const IN_SELECT_END: &[&str] = &["input", "keygen", "textarea"];
const IN_SELECT_TABLE_END: &[&str] = &[
    "caption", "table", "tbody", "td", "tfoot", "th", "thead", "tr",
];
const IN_TABLE_END_IGNORE: &[&str] = &["tbody", "tfoot", "thead"];
const IN_CAPTION_IGNORE: &[&str] = &[
    "body", "col", "colgroup", "html", "tbody", "td", "tfoot", "th", "thead", "tr",
];
const IN_TEMPLATE_TO_HEAD: &[&str] = &[
    "base", "basefont", "bgsound", "link", "meta", "noframes", "script", "style", "template",
    "title",
];
const IN_TEMPLATE_TO_TABLE: &[&str] = &["caption", "colgroup", "tbody", "tfoot", "thead"];
const IN_FOREIGN_TO_HTML: &[&str] = &[
    "b",
    "big",
    "blockquote",
    "body",
    "br",
    "center",
    "code",
    "dd",
    "div",
    "dl",
    "dt",
    "em",
    "embed",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "hr",
    "i",
    "img",
    "li",
    "listing",
    "menu",
    "meta",
    "nobr",
    "ol",
    "p",
    "pre",
    "ruby",
    "s",
    "small",
    "span",
    "strike",
    "strong",
    "sub",
    "sup",
    "table",
    "tt",
    "u",
    "ul",
    "var",
];
const MATHML_TEXT_INTEGRATION: &[&str] = &["mi", "mn", "mo", "ms", "mtext"];
const SVG_HTML_INTEGRATION: &[&str] = &["desc", "foreignObject", "title"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Initial,
    BeforeHtml,
    BeforeHead,
    InHead,
    AfterHead,
    InBody,
    Text,
    InTable,
    InTableText,
    InCaption,
    InColumnGroup,
    InTableBody,
    InRow,
    InCell,
    InSelect,
    InSelectInTable,
    InTemplate,
    AfterBody,
    InFrameset,
    AfterFrameset,
    AfterAfterBody,
    AfterAfterFrameset,
    ForeignContent,
}

struct NoscriptIsland {
    boundary: NodeId,
    saved_form: Option<NodeId>,
}

/// Builds the document jsoup builds from `text`, read as `source` reads it.
pub(super) fn parse(text: &[u16], source: Source) -> Dom {
    let mut builder = TreeBuilder {
        tokeniser: Tokeniser::new(CharacterReader::new(text, source)),
        dom: Dom::new(TagSet::tika()),
        stack: Vec::new(),
        mode: Mode::Initial,
        original_mode: None,
        head: None,
        form: None,
        formatting: Vec::new(),
        template_modes: Vec::new(),
        noscript: None,
        pending_table_characters: Vec::new(),
        frameset_ok: true,
        foster_inserts: false,
    };
    loop {
        let cdata_allowed = builder
            .stack
            .last()
            .is_some_and(|&current| builder.dom.namespace(current) != Namespace::Html);
        let mut token = builder.tokeniser.read(cdata_allowed);
        let eof = matches!(token, Token::Eof);
        builder.process(&mut token);
        if eof {
            return builder.dom;
        }
    }
}

struct TreeBuilder<'a> {
    tokeniser: Tokeniser<'a>,
    dom: Dom,
    stack: Vec<NodeId>,
    mode: Mode,
    original_mode: Option<Mode>,
    head: Option<NodeId>,
    form: Option<NodeId>,
    /// The active formatting elements; `None` is a marker.
    formatting: Vec<Option<NodeId>>,
    template_modes: Vec<Mode>,
    noscript: Option<NoscriptIsland>,
    pending_table_characters: Vec<Vec<u16>>,
    frameset_ok: bool,
    foster_inserts: bool,
}

fn whitespace(token: &Token) -> bool {
    matches!(token, Token::Character { data, .. } if is_blank(data))
}

fn tag_mut(token: &mut Token) -> &mut TagToken {
    match token {
        Token::Tag(tag) => tag,
        other => panic!("expected a tag token, got {other:?}"),
    }
}

impl<'a> TreeBuilder<'a> {
    fn process(&mut self, token: &mut Token) -> bool {
        if self.noscript.is_some() && self.mode != Mode::Text {
            return self.process_noscript_token(token);
        }
        let mode = if self.use_current_or_foreign_insert(token) {
            self.mode
        } else {
            Mode::ForeignContent
        };
        self.process_in(token, mode)
    }

    fn process_in(&mut self, token: &mut Token, mode: Mode) -> bool {
        match mode {
            Mode::Initial => self.initial(token),
            Mode::BeforeHtml => self.before_html(token),
            Mode::BeforeHead => self.before_head(token),
            Mode::InHead => self.in_head(token),
            Mode::AfterHead => self.after_head(token),
            Mode::InBody => self.in_body(token),
            Mode::Text => self.text(token),
            Mode::InTable => self.in_table(token),
            Mode::InTableText => self.in_table_text(token),
            Mode::InCaption => self.in_caption(token),
            Mode::InColumnGroup => self.in_column_group(token),
            Mode::InTableBody => self.in_table_body(token),
            Mode::InRow => self.in_row(token),
            Mode::InCell => self.in_cell(token),
            Mode::InSelect => self.in_select(token),
            Mode::InSelectInTable => self.in_select_in_table(token),
            Mode::InTemplate => self.in_template(token),
            Mode::AfterBody => self.after_body(token),
            Mode::InFrameset => self.in_frameset(token),
            Mode::AfterFrameset => self.after_frameset(token),
            Mode::AfterAfterBody => self.after_after_body(token),
            Mode::AfterAfterFrameset => self.after_after_frameset(token),
            Mode::ForeignContent => self.foreign_content(token),
        }
    }

    fn process_start_tag(&mut self, name: &str) -> bool {
        self.process(&mut Token::Tag(TagToken::named(true, name)))
    }

    fn process_end_tag(&mut self, name: &str) -> bool {
        self.process(&mut Token::Tag(TagToken::named(false, name)))
    }

    fn transition(&mut self, mode: Mode) {
        self.mode = mode;
    }

    fn mark_insertion_mode(&mut self) {
        self.original_mode = Some(self.mode);
    }

    fn original_mode(&self) -> Mode {
        self.original_mode
            .unwrap_or_else(|| panic!("jsoup's tree builder returned to an unset mode"))
    }

    // The stack of open elements.

    fn current(&self) -> NodeId {
        *self
            .stack
            .last()
            .unwrap_or_else(|| panic!("jsoup's tree builder read an empty stack"))
    }

    fn current_el_or_doc(&self) -> NodeId {
        self.stack.last().copied().unwrap_or(DOCUMENT)
    }

    fn current_element_is(&self, name: &str) -> bool {
        self.stack
            .last()
            .is_some_and(|&current| self.dom.is_html(current, name))
    }

    fn current_element_is_in(&self, name: &str, namespace: Namespace) -> bool {
        self.stack.last().is_some_and(|&current| {
            self.dom.normal_name(current) == name && self.dom.namespace(current) == namespace
        })
    }

    fn pop(&mut self) -> NodeId {
        self.stack
            .pop()
            .unwrap_or_else(|| panic!("jsoup's tree builder popped an empty stack"))
    }

    fn push(&mut self, element: NodeId) {
        self.stack.push(element);
    }

    fn get_from_stack(&self, name: &str) -> Option<NodeId> {
        self.stack
            .iter()
            .rev()
            .copied()
            .find(|&element| self.dom.is_html(element, name))
    }

    fn on_stack(&self, name: &str) -> bool {
        self.get_from_stack(name).is_some()
    }

    fn on_stack_node(&self, element: NodeId) -> bool {
        self.stack.contains(&element)
    }

    fn remove_from_stack(&mut self, element: NodeId) -> bool {
        match self.stack.iter().rposition(|&open| open == element) {
            Some(index) => {
                self.stack.remove(index);
                true
            }
            None => false,
        }
    }

    fn pop_stack_to_close(&mut self, name: &str) -> Option<NodeId> {
        for _ in 0..self.stack.len() {
            let element = self.pop();
            if self.dom.is_html(element, name) {
                return Some(element);
            }
        }
        None
    }

    fn pop_stack_to_close_any_namespace(&mut self, name: &str) {
        for _ in 0..self.stack.len() {
            let element = self.pop();
            if self.dom.name_is(element, name) {
                return;
            }
        }
    }

    fn pop_stack_to_close_any(&mut self, names: &[&str]) {
        for _ in 0..self.stack.len() {
            let element = self.pop();
            if names.contains(&self.dom.normal_name(element))
                && self.dom.namespace(element) == Namespace::Html
            {
                break;
            }
        }
    }

    fn clear_stack_to_context(&mut self, names: &[&str]) {
        while let Some(&element) = self.stack.last() {
            if self.dom.namespace(element) == Namespace::Html
                && (names.contains(&self.dom.normal_name(element))
                    || self.dom.name_is(element, "html"))
            {
                break;
            }
            self.pop();
        }
    }

    fn clear_stack_to_table_context(&mut self) {
        self.clear_stack_to_context(&["table", "template"]);
    }

    fn clear_stack_to_table_body_context(&mut self) {
        self.clear_stack_to_context(&["tbody", "tfoot", "thead", "template"]);
    }

    fn clear_stack_to_table_row_context(&mut self) {
        self.clear_stack_to_context(&["tr", "template"]);
    }

    fn above_on_stack(&self, element: NodeId) -> Option<NodeId> {
        if !self.on_stack_node(element) {
            return None;
        }
        for pos in (1..self.stack.len()).rev() {
            if self.stack[pos] == element {
                return Some(self.stack[pos - 1]);
            }
        }
        None
    }

    fn insert_on_stack_after(&mut self, after: NodeId, element: NodeId) {
        match self.stack.iter().rposition(|&open| open == after) {
            Some(index) => self.stack.insert(index + 1, element),
            None => self.stack.push(element),
        }
    }

    fn replace_on_stack(&mut self, out: NodeId, element: NodeId) {
        let index = self
            .stack
            .iter()
            .rposition(|&open| open == out)
            .unwrap_or_else(|| panic!("jsoup's tree builder replaced an element not on the stack"));
        self.stack[index] = element;
    }

    fn reset_insertion_mode(&mut self) -> bool {
        let original = self.mode;
        if self.stack.is_empty() {
            self.transition(Mode::InBody);
        }
        for pos in (0..self.stack.len()).rev() {
            let last = pos == 0;
            let node = self.stack[pos];
            let name = if self.dom.namespace(node) == Namespace::Html {
                self.dom.normal_name(node).to_string()
            } else {
                String::new()
            };
            match name.as_str() {
                "select" => {
                    self.transition(Mode::InSelect);
                    break;
                }
                "td" | "th" if !last => {
                    self.transition(Mode::InCell);
                    break;
                }
                "tr" => {
                    self.transition(Mode::InRow);
                    break;
                }
                "tbody" | "thead" | "tfoot" => {
                    self.transition(Mode::InTableBody);
                    break;
                }
                "caption" => {
                    self.transition(Mode::InCaption);
                    break;
                }
                "colgroup" => {
                    self.transition(Mode::InColumnGroup);
                    break;
                }
                "table" => {
                    self.transition(Mode::InTable);
                    break;
                }
                "template" => {
                    let mode = *self
                        .template_modes
                        .last()
                        .unwrap_or_else(|| panic!("no template insertion mode on the stack"));
                    self.transition(mode);
                    break;
                }
                "head" if !last => {
                    self.transition(Mode::InHead);
                    break;
                }
                "body" => {
                    self.transition(Mode::InBody);
                    break;
                }
                "frameset" => {
                    self.transition(Mode::InFrameset);
                    break;
                }
                "html" => {
                    self.transition(if self.head.is_none() {
                        Mode::BeforeHead
                    } else {
                        Mode::AfterHead
                    });
                    break;
                }
                _ => {}
            }
            if last {
                self.transition(Mode::InBody);
                break;
            }
        }
        self.mode != original
    }

    fn reset_body(&mut self) {
        if !self.on_stack("body") {
            let body = self.document_body();
            self.stack.push(body);
        }
        self.transition(Mode::InBody);
    }

    /// The document's `body` or `frameset`, made when there is none (`Document.body`).
    fn document_body(&mut self) -> NodeId {
        let html = match self.first_child_named(DOCUMENT, &["html"]) {
            Some(html) => html,
            None => self.append_new_element(DOCUMENT, "html"),
        };
        match self.first_child_named(html, &["body", "frameset"]) {
            Some(body) => body,
            None => self.append_new_element(html, "body"),
        }
    }

    fn first_child_named(&self, parent: NodeId, names: &[&str]) -> Option<NodeId> {
        self.dom
            .node(parent)
            .children
            .iter()
            .copied()
            .find(|&child| {
                matches!(self.dom.node(child).data, NodeData::Element { .. })
                    && names.contains(&self.dom.normal_name(child))
            })
    }

    fn append_new_element(&mut self, parent: NodeId, name: &str) -> NodeId {
        let tag = self.dom.tags.value_of(name, None, Namespace::Html, false);
        let element = self.dom.create(NodeData::Element {
            tag,
            attributes: Vec::new(),
        });
        self.dom.append(parent, element);
        element
    }

    fn in_specific_scope(&self, target: &str, boundary: u32) -> bool {
        for &element in self.stack.iter().rev() {
            let tag = self.dom.tag(element);
            if tag.namespace == Namespace::Html && tag.normal == target {
                return true;
            }
            if tag.has_parser_option(boundary) {
                return false;
            }
        }
        false
    }

    fn has_heading_in_scope(&self) -> bool {
        for &element in self.stack.iter().rev() {
            let tag = self.dom.tag(element);
            if tag.namespace == Namespace::Html && HEADINGS.contains(&tag.normal.as_str()) {
                return true;
            }
            if tag.has_parser_option(SCOPE) {
                return false;
            }
        }
        false
    }

    fn in_scope(&self, target: &str) -> bool {
        self.in_specific_scope(target, SCOPE)
    }

    fn in_list_item_scope(&self, target: &str) -> bool {
        self.in_specific_scope(target, SCOPE | LIST_SCOPE)
    }

    fn in_button_scope(&self, target: &str) -> bool {
        self.in_specific_scope(target, SCOPE | BUTTON_SCOPE)
    }

    fn in_table_scope(&self, target: &str) -> bool {
        self.in_specific_scope(target, TABLE_SCOPE)
    }

    fn in_select_scope(&self, target: &str) -> bool {
        for &element in self.stack.iter().rev() {
            if self.dom.normal_name(element) == target {
                return true;
            }
            if !self.dom.tag(element).has_parser_option(SELECT_SCOPE_MEMBER) {
                return false;
            }
        }
        false
    }

    fn generate_implied_end_tags_except(&mut self, exclude: &str) {
        while let Some(&current) = self.stack.last() {
            if !self.dom.tag(current).has_parser_option(IMPLIED_END) {
                break;
            }
            if self.current_element_is(exclude) {
                break;
            }
            self.pop();
        }
    }

    fn generate_implied_end_tags(&mut self, thorough: bool) {
        let option = if thorough {
            THOROUGH_IMPLIED_END
        } else {
            IMPLIED_END
        };
        while let Some(&current) = self.stack.last() {
            if !self.dom.tag(current).has_parser_option(option) {
                break;
            }
            self.pop();
        }
    }

    fn close_element(&mut self, name: &str) {
        self.generate_implied_end_tags_except(name);
        self.pop_stack_to_close(name);
    }

    fn is_special(&self, element: NodeId) -> bool {
        self.dom.tag(element).has_parser_option(SPECIAL)
    }

    fn is_mathml_text_integration(&self, element: NodeId) -> bool {
        self.dom.namespace(element) == Namespace::MathMl
            && MATHML_TEXT_INTEGRATION.contains(&self.dom.normal_name(element))
    }

    fn is_html_integration(&self, element: NodeId) -> bool {
        let tag = self.dom.tag(element);
        if tag.namespace == Namespace::MathMl && tag.normal == "annotation-xml" {
            let encoding = self
                .dom
                .attr(element, "encoding")
                .to_lowercase()
                .trim_matches(|c: char| c <= ' ')
                .to_string();
            if encoding == "text/html" || encoding == "application/xhtml+xml" {
                return true;
            }
        }
        tag.namespace == Namespace::Svg && SVG_HTML_INTEGRATION.contains(&tag.name.as_str())
    }

    fn use_current_or_foreign_insert(&self, token: &Token) -> bool {
        let Some(&current) = self.stack.last() else {
            return true;
        };
        let namespace = self.dom.namespace(current);
        if namespace == Namespace::Html {
            return true;
        }
        if self.is_mathml_text_integration(current) {
            if let Token::Tag(tag) = token {
                if tag.start && tag.normal_name() != "mglyph" && tag.normal_name() != "malignmark" {
                    return true;
                }
            }
            if matches!(token, Token::Character { .. }) {
                return true;
            }
        }
        if namespace == Namespace::MathMl
            && self.dom.name_is(current, "annotation-xml")
            && token.is_start_named("svg")
        {
            return true;
        }
        if self.is_html_integration(current)
            && (token.is_start_tag() || matches!(token, Token::Character { .. }))
        {
            return true;
        }
        matches!(token, Token::Eof)
    }

    // Making and inserting nodes.

    /// The element a start tag makes, its attributes normalized and deduplicated in the token as
    /// jsoup does (`createElementFor`).
    fn create_element_for(
        &mut self,
        start: &mut TagToken,
        namespace: Namespace,
        preserve_case: bool,
    ) -> NodeId {
        let mut attributes = Vec::new();
        if let Some(raw) = start.attributes.as_mut() {
            if !raw.is_empty() {
                if !preserve_case {
                    for attribute in raw.iter_mut() {
                        attribute.name = string(&attribute.name)
                            .to_lowercase()
                            .encode_utf16()
                            .collect();
                    }
                }
                let mut i = 0;
                while i < raw.len() {
                    let mut j = i + 1;
                    while j < raw.len() {
                        let equal = if preserve_case {
                            raw[i].name == raw[j].name
                        } else {
                            equals_ignore_case(&raw[i].name, &raw[j].name)
                        };
                        if equal {
                            raw.remove(j);
                        } else {
                            j += 1;
                        }
                    }
                    i += 1;
                }
            }
            attributes = raw
                .iter()
                .map(|attribute| Attribute {
                    key: string(&attribute.name),
                    value: attribute.value.as_deref().map(string),
                })
                .collect();
        }
        let name = string(start.name());
        let normal = start.normal_name().to_string();
        let tag = self
            .dom
            .tags
            .value_of(&name, Some(&normal), namespace, preserve_case);
        self.dom.create(NodeData::Element { tag, attributes })
    }

    fn insert_element_for(&mut self, start: &mut TagToken) -> NodeId {
        let element = self.create_element_for(start, Namespace::Html, false);
        self.do_insert_element(element);
        if start.self_closing {
            let tag = self.dom.tag(element);
            if !tag.is_void() && tag.is_known() && tag.is_self_closing() {
                let name = tag.name.clone();
                self.tokeniser.transition(State::Data);
                self.tokeniser
                    .emit(Token::Tag(TagToken::named(false, &name)));
            }
        }
        if self.dom.tag(element).is_void() {
            self.pop();
        }
        element
    }

    fn insert_foreign_element_for(&mut self, start: &mut TagToken, namespace: Namespace) {
        let element = self.create_element_for(start, namespace, true);
        self.do_insert_element(element);
        if start.self_closing {
            self.pop();
        }
    }

    fn insert_empty_element_for(&mut self, start: &mut TagToken) -> NodeId {
        let element = self.create_element_for(start, Namespace::Html, false);
        self.do_insert_element(element);
        self.pop();
        element
    }

    fn insert_form_element(&mut self, start: &mut TagToken, on_stack: bool, check_template: bool) {
        let element = self.create_element_for(start, Namespace::Html, false);
        if !check_template || !self.on_stack("template") {
            self.form = Some(element);
        }
        self.do_insert_element(element);
        if !on_stack {
            self.pop();
        }
    }

    fn do_insert_element(&mut self, element: NodeId) {
        while self.stack.len() >= MAX_DEPTH {
            let trimmed = self.pop();
            self.on_stack_pruned_for_depth(trimmed);
        }
        let target = self.current_el_or_doc();
        if self.foster_inserts && IN_TABLE_FOSTER.contains(&self.dom.normal_name(target)) {
            self.insert_in_foster_parent(element);
        } else {
            self.dom.append(target, element);
        }
        self.push(element);
    }

    fn on_stack_pruned_for_depth(&mut self, element: NodeId) {
        if self.head == Some(element) {
            self.head = None;
        }
        if self.form == Some(element) {
            self.form = None;
        }
        self.remove_from_active_formatting_elements(element);
        if self.dom.name_is(element, "template") {
            self.clear_formatting_elements_to_last_marker();
            self.template_modes.pop();
            self.reset_insertion_mode();
        } else if self
            .noscript
            .as_ref()
            .is_some_and(|island| island.boundary == element)
        {
            self.restore_noscript();
        }
    }

    fn insert_comment_node(&mut self, data: &[u16], target: NodeId) {
        let comment = self.dom.create(NodeData::Comment(string(data)));
        self.dom.append(target, comment);
    }

    fn insert_comment(&mut self, data: &[u16]) {
        let target = self.current_el_or_doc();
        self.insert_comment_node(data, target);
    }

    /// Inserts text into the current element, its NULs removed, or with `replace` replaced by
    /// U+FFFD (`insertCharacterNode`).
    fn insert_character_node(&mut self, token: &mut Token, replace: bool) {
        if let Token::Character { data, .. } = token {
            if data.contains(&0) {
                if replace {
                    for unit in data.iter_mut().filter(|unit| **unit == 0) {
                        *unit = 0xFFFD;
                    }
                } else {
                    data.retain(|&unit| unit != 0);
                }
            }
        }
        let target = self.current_el_or_doc();
        self.insert_character_to_element(token, target);
    }

    fn insert_character_to_element(&mut self, token: &Token, element: NodeId) {
        let Token::Character { data, cdata } = token else {
            panic!("expected a character token, got {token:?}");
        };
        let text = string(data);
        let node = if *cdata {
            NodeData::CData(text)
        } else if self.dom.tag(element).is(DATA) {
            NodeData::Data(text)
        } else {
            NodeData::Text(text)
        };
        let node = self.dom.create(node);
        self.dom.append(element, node);
    }

    fn insert_in_foster_parent(&mut self, node: NodeId) {
        match self.get_from_stack("table") {
            Some(table) => match self.dom.parent(table) {
                Some(_) => self.dom.insert_before(table, node),
                None => {
                    let parent = self
                        .above_on_stack(table)
                        .unwrap_or_else(|| panic!("the last table has no foster parent"));
                    self.dom.append(parent, node);
                }
            },
            None => {
                let parent = self.stack[0];
                self.dom.append(parent, node);
            }
        }
    }

    // Active formatting elements.

    fn last_formatting_element(&self) -> Option<NodeId> {
        self.formatting.last().copied().flatten()
    }

    fn position_of_element(&self, element: NodeId) -> Option<usize> {
        self.formatting
            .iter()
            .position(|&entry| entry == Some(element))
    }

    fn push_active_formatting_elements(&mut self, element: NodeId) {
        self.check_active_formatting_elements(element);
        self.formatting.push(Some(element));
    }

    /// Adds `element` to the active formatting elements at `bookmark`, or at the end when
    /// there is no bookmark or it lies past the end.
    fn push_with_bookmark(&mut self, element: NodeId, bookmark: Option<usize>) {
        self.check_active_formatting_elements(element);
        match bookmark {
            Some(bookmark) if bookmark <= self.formatting.len() => {
                self.formatting.insert(bookmark, Some(element));
            }
            _ => self.formatting.push(Some(element)),
        }
    }

    fn check_active_formatting_elements(&mut self, element: NodeId) {
        let mut seen = 0;
        let size = self.formatting.len() as isize - 1;
        let ceil = (size - MAX_USED_FORMATTING_ELEMENTS as isize).max(0);
        let mut pos = size;
        while pos >= ceil {
            let Some(entry) = self.formatting[pos as usize] else {
                break;
            };
            if self.is_same_formatting_element(element, entry) {
                seen += 1;
            }
            if seen == 3 {
                self.formatting.remove(pos as usize);
                break;
            }
            pos -= 1;
        }
    }

    fn is_same_formatting_element(&self, a: NodeId, b: NodeId) -> bool {
        if self.dom.normal_name(a) != self.dom.normal_name(b) {
            return false;
        }
        let (a, b) = (self.dom.attributes(a), self.dom.attributes(b));
        a.len() == b.len()
            && a.iter().all(|attribute| {
                b.iter()
                    .find(|other| other.key == attribute.key)
                    .is_some_and(|other| other.value == attribute.value)
            })
    }

    fn reconstruct_formatting_elements(&mut self) {
        let Some(last) = self.last_formatting_element() else {
            return;
        };
        if self.on_stack_node(last) {
            return;
        }
        let size = self.formatting.len();
        let ceil = size.saturating_sub(MAX_USED_FORMATTING_ELEMENTS);
        let mut pos = size - 1;
        let mut skip = false;
        loop {
            if pos == ceil {
                skip = true;
                break;
            }
            pos -= 1;
            match self.formatting[pos] {
                None => break,
                Some(entry) if self.on_stack_node(entry) => break,
                Some(_) => {}
            }
        }
        loop {
            if !skip {
                pos += 1;
            }
            skip = false;
            let entry =
                self.formatting[pos].unwrap_or_else(|| panic!("jsoup reconstructed a marker"));
            let element = self.recreate_element(entry);
            self.do_insert_element(element);
            self.formatting[pos] = Some(element);
            if pos == size - 1 {
                break;
            }
        }
    }

    fn recreate_element(&mut self, source: NodeId) -> NodeId {
        let tag = self.dom.tag_id(source);
        let attributes = self.dom.attributes(source).to_vec();
        self.dom.create(NodeData::Element { tag, attributes })
    }

    fn clear_formatting_elements_to_last_marker(&mut self) {
        while let Some(entry) = self.formatting.pop() {
            if entry.is_none() {
                break;
            }
        }
    }

    fn remove_from_active_formatting_elements(&mut self, element: NodeId) {
        if let Some(pos) = self
            .formatting
            .iter()
            .rposition(|&entry| entry == Some(element))
        {
            self.formatting.remove(pos);
        }
    }

    fn is_in_active_formatting_elements(&self, element: NodeId) -> bool {
        self.formatting.contains(&Some(element))
    }

    fn get_active_formatting_element(&self, name: &str) -> Option<NodeId> {
        for entry in self.formatting.iter().rev() {
            match entry {
                None => return None,
                Some(element) if self.dom.name_is(*element, name) => return Some(*element),
                Some(_) => {}
            }
        }
        None
    }

    fn replace_active_formatting_element(&mut self, out: NodeId, element: NodeId) {
        let index = self
            .formatting
            .iter()
            .rposition(|&entry| entry == Some(out))
            .unwrap_or_else(|| panic!("jsoup replaced an inactive formatting element"));
        self.formatting[index] = Some(element);
    }

    // The noscript island.

    fn start_noscript(&mut self, start: &mut TagToken) {
        let boundary = self.insert_element_for(start);
        self.noscript = Some(NoscriptIsland {
            boundary,
            saved_form: self.form,
        });
        self.form = None;
    }

    fn in_noscript_scope(&self, name: &str) -> bool {
        let Some(island) = &self.noscript else {
            return false;
        };
        for &element in self.stack.iter().rev() {
            if element == island.boundary {
                return false;
            }
            if self.dom.name_is(element, name) {
                return true;
            }
        }
        false
    }

    fn end_noscript(&mut self) {
        let boundary = self
            .noscript
            .as_ref()
            .unwrap_or_else(|| panic!("noscript closed with no island"))
            .boundary;
        if let Some(index) = self.stack.iter().rposition(|&element| element == boundary) {
            while self.stack.len() > index {
                self.pop();
            }
        }
        self.restore_noscript();
    }

    fn restore_noscript(&mut self) {
        let island = self
            .noscript
            .take()
            .unwrap_or_else(|| panic!("no noscript island to restore"));
        self.form = island.saved_form;
    }

    fn process_noscript_token(&mut self, token: &mut Token) -> bool {
        match token {
            Token::Tag(tag) if tag.start => {
                let text_state = self.text_state_for(tag);
                let element = self.insert_element_for(tag);
                if let Some(state) = text_state {
                    if tag.self_closing {
                        if self.stack.last() == Some(&element) {
                            self.pop();
                        }
                    } else {
                        self.tokeniser.transition(state);
                        self.mark_insertion_mode();
                        self.transition(Mode::Text);
                    }
                }
                self.frameset_ok = false;
                true
            }
            Token::Tag(tag) => {
                let name = tag.normal_name().to_string();
                if name == "noscript" {
                    self.end_noscript();
                    return true;
                }
                if !self.in_noscript_scope(&name) {
                    return false;
                }
                self.pop_stack_to_close(&name);
                true
            }
            Token::Comment(data) => {
                let data = data.clone();
                self.insert_comment(&data);
                true
            }
            Token::Character { .. } => {
                self.insert_character_node(token, false);
                if !whitespace(token) {
                    self.frameset_ok = false;
                }
                true
            }
            Token::Doctype(_) => false,
            Token::Eof => {
                self.end_noscript();
                self.process(token)
            }
        }
    }

    /// The tokeniser state a tag's content takes (`Tag.textState`), adding the tag to the set
    /// as jsoup's lookup does.
    fn text_state_for(&mut self, tag: &TagToken) -> Option<State> {
        let name = string(tag.name());
        let id = self
            .dom
            .tags
            .value_of(&name, Some(tag.normal_name()), Namespace::Html, false);
        let tag = self.dom.tags.tag(id);
        if tag.is(RCDATA) {
            Some(State::Rcdata)
        } else if tag.is(DATA) {
            Some(State::Rawtext)
        } else {
            None
        }
    }

    fn handle_text_state(&mut self, start: &mut TagToken, state: Option<State>) {
        if let Some(state) = state {
            self.tokeniser.transition(state);
        }
        self.mark_insertion_mode();
        self.transition(Mode::Text);
        self.insert_element_for(start);
    }

    fn merge_attributes(&mut self, source: &TagToken, dest: NodeId) {
        let Some(attributes) = &source.attributes else {
            return;
        };
        for attribute in attributes {
            let key = string(&attribute.name);
            let exists = self
                .dom
                .attributes(dest)
                .iter()
                .any(|existing| existing.key == key);
            if !exists {
                self.dom.attributes_mut(dest).push(Attribute {
                    key,
                    value: attribute.value.as_deref().map(string),
                });
            }
        }
    }

    // The insertion modes.

    fn initial(&mut self, token: &mut Token) -> bool {
        if whitespace(token) {
            return true;
        }
        match token {
            Token::Comment(data) => {
                let data = data.clone();
                self.insert_comment(&data);
            }
            Token::Doctype(doctype) => {
                let name = string(doctype.name.value())
                    .trim_matches(|c: char| c <= ' ')
                    .to_lowercase();
                let public_id = string(doctype.public_identifier.value());
                let system_id = string(doctype.system_identifier.value());
                let quirks = doctype.force_quirks
                    || name != "html"
                    || equals_ignore_case(
                        &public_id.encode_utf16().collect::<Vec<_>>(),
                        &"HTML".encode_utf16().collect::<Vec<_>>(),
                    );
                let node = self.dom.create(NodeData::Doctype {
                    name,
                    public_id,
                    system_id,
                });
                self.dom.append(DOCUMENT, node);
                if quirks {
                    self.dom.quirks = true;
                }
                self.transition(Mode::BeforeHtml);
            }
            _ => {
                self.dom.quirks = true;
                self.transition(Mode::BeforeHtml);
                return self.process(token);
            }
        }
        true
    }

    fn before_html(&mut self, token: &mut Token) -> bool {
        match token {
            Token::Doctype(_) => false,
            Token::Comment(data) => {
                let data = data.clone();
                self.insert_comment(&data);
                true
            }
            _ if whitespace(token) => {
                self.insert_character_node(token, false);
                true
            }
            Token::Tag(tag) if tag.start && tag.normal_name() == "html" => {
                self.insert_element_for(tag);
                self.transition(Mode::BeforeHead);
                true
            }
            Token::Tag(tag) if !tag.start && BEFORE_HTML_TO_HEAD.contains(&tag.normal_name()) => {
                self.before_html_anything_else(token)
            }
            Token::Tag(tag) if !tag.start => false,
            _ => self.before_html_anything_else(token),
        }
    }

    fn before_html_anything_else(&mut self, token: &mut Token) -> bool {
        self.process_start_tag("html");
        self.transition(Mode::BeforeHead);
        self.process(token)
    }

    fn before_head(&mut self, token: &mut Token) -> bool {
        if whitespace(token) {
            self.insert_character_node(token, false);
            return true;
        }
        match token {
            Token::Comment(data) => {
                let data = data.clone();
                self.insert_comment(&data);
                true
            }
            Token::Doctype(_) => false,
            Token::Tag(tag) if tag.start && tag.normal_name() == "html" => {
                self.process_in(token, Mode::InBody)
            }
            Token::Tag(tag) if tag.start && tag.normal_name() == "head" => {
                let head = self.insert_element_for(tag);
                self.head = Some(head);
                self.transition(Mode::InHead);
                true
            }
            Token::Tag(tag) if !tag.start && BEFORE_HTML_TO_HEAD.contains(&tag.normal_name()) => {
                self.process_start_tag("head");
                self.process(token)
            }
            Token::Tag(tag) if !tag.start => false,
            _ => {
                self.process_start_tag("head");
                self.process(token)
            }
        }
    }

    fn in_head(&mut self, token: &mut Token) -> bool {
        if whitespace(token) {
            self.insert_character_node(token, false);
            return true;
        }
        match token {
            Token::Comment(data) => {
                let data = data.clone();
                self.insert_comment(&data);
            }
            Token::Doctype(_) => return false,
            Token::Tag(tag) if tag.start => {
                let name = tag.normal_name().to_string();
                if name == "html" {
                    return self.process_in(token, Mode::InBody);
                } else if IN_HEAD_EMPTY.contains(&name.as_str()) || name == "meta" {
                    self.insert_empty_element_for(tag);
                } else if name == "title" || IN_HEAD_RAW.contains(&name.as_str()) {
                    let state = self.text_state_for(tag);
                    self.handle_text_state(tag, state);
                } else if name == "noscript" {
                    self.start_noscript(tag);
                } else if name == "script" {
                    self.handle_text_state(tag, Some(State::ScriptData));
                } else if name == "head" {
                    return false;
                } else if name == "template" {
                    self.insert_element_for(tag);
                    self.formatting.push(None);
                    self.frameset_ok = false;
                    self.transition(Mode::InTemplate);
                    self.template_modes.push(Mode::InTemplate);
                } else {
                    return self.in_head_anything_else(token);
                }
            }
            Token::Tag(tag) => {
                let name = tag.normal_name().to_string();
                if name == "head" {
                    self.pop();
                    self.transition(Mode::AfterHead);
                } else if IN_HEAD_END.contains(&name.as_str()) {
                    return self.in_head_anything_else(token);
                } else if name == "template" {
                    if self.on_stack(&name) {
                        self.generate_implied_end_tags(true);
                        self.pop_stack_to_close(&name);
                        self.clear_formatting_elements_to_last_marker();
                        self.template_modes.pop();
                        self.reset_insertion_mode();
                    }
                } else {
                    return false;
                }
            }
            _ => return self.in_head_anything_else(token),
        }
        true
    }

    fn in_head_anything_else(&mut self, token: &mut Token) -> bool {
        self.process_end_tag("head");
        self.process(token)
    }

    fn after_head(&mut self, token: &mut Token) -> bool {
        if whitespace(token) {
            self.insert_character_node(token, false);
            return true;
        }
        match token {
            Token::Comment(data) => {
                let data = data.clone();
                self.insert_comment(&data);
            }
            Token::Doctype(_) => {}
            Token::Tag(tag) if tag.start => {
                let name = tag.normal_name().to_string();
                if name == "html" {
                    return self.process_in(token, Mode::InBody);
                } else if name == "body" {
                    self.insert_element_for(tag);
                    self.frameset_ok = false;
                    self.transition(Mode::InBody);
                } else if name == "frameset" {
                    self.insert_element_for(tag);
                    self.transition(Mode::InFrameset);
                } else if IN_BODY_START_TO_HEAD.contains(&name.as_str()) {
                    let head = self
                        .head
                        .unwrap_or_else(|| panic!("jsoup is after a head it does not have"));
                    self.push(head);
                    self.process_in(token, Mode::InHead);
                    self.remove_from_stack(head);
                } else if name == "head" {
                    return false;
                } else {
                    self.after_head_anything_else(token);
                }
            }
            Token::Tag(tag) => {
                let name = tag.normal_name();
                if AFTER_HEAD_BODY.contains(&name) {
                    self.after_head_anything_else(token);
                } else if name == "template" {
                    self.process_in(token, Mode::InHead);
                } else {
                    return false;
                }
            }
            _ => {
                self.after_head_anything_else(token);
            }
        }
        true
    }

    fn after_head_anything_else(&mut self, token: &mut Token) -> bool {
        self.process_start_tag("body");
        self.frameset_ok = true;
        self.process(token)
    }

    fn in_body(&mut self, token: &mut Token) -> bool {
        match token {
            Token::Character { data, .. } => {
                let blank = is_blank(data);
                if self.frameset_ok && blank {
                    self.reconstruct_formatting_elements();
                    self.insert_character_node(token, false);
                } else {
                    self.reconstruct_formatting_elements();
                    self.insert_character_node(token, false);
                    self.frameset_ok = false;
                }
                true
            }
            Token::Comment(data) => {
                let data = data.clone();
                self.insert_comment(&data);
                true
            }
            Token::Doctype(_) => false,
            Token::Tag(tag) if tag.start => self.in_body_start_tag(token),
            Token::Tag(_) => self.in_body_end_tag(token),
            Token::Eof => {
                if !self.template_modes.is_empty() {
                    return self.process_in(token, Mode::InTemplate);
                }
                true
            }
        }
    }

    fn in_body_start_tag(&mut self, token: &mut Token) -> bool {
        let name = tag_mut(token).normal_name().to_string();
        match name.as_str() {
            "a" => {
                if self.get_active_formatting_element("a").is_some() {
                    self.process_end_tag("a");
                    if let Some(remaining) = self.get_from_stack("a") {
                        self.remove_from_active_formatting_elements(remaining);
                        self.remove_from_stack(remaining);
                    }
                }
                self.reconstruct_formatting_elements();
                let element = self.insert_element_for(tag_mut(token));
                self.push_active_formatting_elements(element);
            }
            "span" => {
                self.reconstruct_formatting_elements();
                self.insert_element_for(tag_mut(token));
            }
            "li" => {
                self.frameset_ok = false;
                for i in (1..self.stack.len()).rev() {
                    let element = self.stack[i];
                    if self.dom.name_is(element, "li") {
                        self.process_end_tag("li");
                        break;
                    }
                    if self.is_special(element)
                        && !IN_BODY_START_LI_BREAKERS.contains(&self.dom.normal_name(element))
                    {
                        break;
                    }
                }
                if self.in_button_scope("p") {
                    self.process_end_tag("p");
                }
                self.insert_element_for(tag_mut(token));
            }
            "html" => {
                if self.on_stack("template") {
                    return false;
                }
                if let Some(&html) = self.stack.first() {
                    let tag = tag_mut(token).clone();
                    self.merge_attributes(&tag, html);
                }
            }
            "body" => {
                let len = self.stack.len();
                if len < 2
                    || (len > 2 && !self.dom.name_is(self.stack[1], "body"))
                    || self.on_stack("template")
                {
                    return false;
                }
                self.frameset_ok = false;
                if let Some(body) = self.get_from_stack("body") {
                    let tag = tag_mut(token).clone();
                    self.merge_attributes(&tag, body);
                }
            }
            "frameset" => {
                let len = self.stack.len();
                if len < 2 || (len > 2 && !self.dom.name_is(self.stack[1], "body")) {
                    return false;
                }
                if !self.frameset_ok {
                    return false;
                }
                let second = self.stack[1];
                if self.dom.parent(second).is_some() {
                    self.dom.remove(second);
                }
                self.stack.truncate(1);
                self.insert_element_for(tag_mut(token));
                self.transition(Mode::InFrameset);
            }
            "form" => {
                if self.form.is_some() && !self.on_stack("template") {
                    return false;
                }
                if self.in_button_scope("p") {
                    self.close_element("p");
                }
                self.insert_form_element(tag_mut(token), true, true);
            }
            "plaintext" => {
                if self.in_button_scope("p") {
                    self.process_end_tag("p");
                }
                self.insert_element_for(tag_mut(token));
                self.tokeniser.transition(State::Plaintext);
            }
            "button" => {
                if self.in_button_scope("button") {
                    self.process_end_tag("button");
                    self.process(token);
                } else {
                    self.reconstruct_formatting_elements();
                    self.insert_element_for(tag_mut(token));
                    self.frameset_ok = false;
                }
            }
            "nobr" => {
                self.reconstruct_formatting_elements();
                if self.in_scope("nobr") {
                    self.process_end_tag("nobr");
                    self.reconstruct_formatting_elements();
                }
                let element = self.insert_element_for(tag_mut(token));
                self.push_active_formatting_elements(element);
            }
            "table" => {
                if !self.dom.quirks && self.in_button_scope("p") {
                    self.process_end_tag("p");
                }
                self.insert_element_for(tag_mut(token));
                self.frameset_ok = false;
                self.transition(Mode::InTable);
            }
            "input" => {
                self.reconstruct_formatting_elements();
                let element = self.insert_empty_element_for(tag_mut(token));
                let hidden = equals_ignore_case(
                    &self
                        .dom
                        .attr(element, "type")
                        .encode_utf16()
                        .collect::<Vec<_>>(),
                    &"hidden".encode_utf16().collect::<Vec<_>>(),
                );
                if !hidden {
                    self.frameset_ok = false;
                }
            }
            "hr" => {
                if self.in_button_scope("p") {
                    self.process_end_tag("p");
                }
                self.insert_empty_element_for(tag_mut(token));
                self.frameset_ok = false;
            }
            "image" => {
                if self.get_from_stack("svg").is_none() {
                    tag_mut(token).set_name("img");
                    return self.process(token);
                }
                self.insert_element_for(tag_mut(token));
            }
            "textarea" => {
                self.frameset_ok = false;
                let state = self.text_state_for(tag_mut(token));
                self.handle_text_state(tag_mut(token), state);
            }
            "xmp" => {
                if self.in_button_scope("p") {
                    self.process_end_tag("p");
                }
                self.reconstruct_formatting_elements();
                self.frameset_ok = false;
                let state = self.text_state_for(tag_mut(token));
                self.handle_text_state(tag_mut(token), state);
            }
            "iframe" => {
                self.frameset_ok = false;
                let state = self.text_state_for(tag_mut(token));
                self.handle_text_state(tag_mut(token), state);
            }
            "noembed" => {
                let state = self.text_state_for(tag_mut(token));
                self.handle_text_state(tag_mut(token), state);
            }
            "noscript" => {
                self.reconstruct_formatting_elements();
                self.start_noscript(tag_mut(token));
            }
            "select" => {
                self.reconstruct_formatting_elements();
                self.insert_element_for(tag_mut(token));
                self.frameset_ok = false;
                if tag_mut(token).self_closing {
                    return true;
                }
                if matches!(
                    self.mode,
                    Mode::InTable
                        | Mode::InCaption
                        | Mode::InTableBody
                        | Mode::InRow
                        | Mode::InCell
                ) {
                    self.transition(Mode::InSelectInTable);
                } else {
                    self.transition(Mode::InSelect);
                }
            }
            "math" => {
                self.reconstruct_formatting_elements();
                self.insert_foreign_element_for(tag_mut(token), Namespace::MathMl);
            }
            "svg" => {
                self.reconstruct_formatting_elements();
                self.insert_foreign_element_for(tag_mut(token), Namespace::Svg);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if self.in_button_scope("p") {
                    self.process_end_tag("p");
                }
                if let Some(&current) = self.stack.last() {
                    if HEADINGS.contains(&self.dom.normal_name(current)) {
                        self.pop();
                    }
                }
                self.insert_element_for(tag_mut(token));
            }
            "pre" | "listing" => {
                if self.in_button_scope("p") {
                    self.process_end_tag("p");
                }
                self.insert_element_for(tag_mut(token));
                self.tokeniser.reader.match_consume("\n");
                self.frameset_ok = false;
            }
            "dd" | "dt" => {
                self.frameset_ok = false;
                let bottom = self.stack.len() as isize - 1;
                let upper = if bottom >= MAX_STACK_SCAN as isize {
                    bottom - MAX_STACK_SCAN as isize
                } else {
                    0
                };
                let mut i = bottom;
                while i >= upper {
                    let element = self.stack[i as usize];
                    let element_name = self.dom.normal_name(element).to_string();
                    if DD_DT.contains(&element_name.as_str()) {
                        self.process_end_tag(&element_name);
                        break;
                    }
                    if self.is_special(element)
                        && !IN_BODY_START_LI_BREAKERS.contains(&element_name.as_str())
                    {
                        break;
                    }
                    i -= 1;
                }
                if self.in_button_scope("p") {
                    self.process_end_tag("p");
                }
                self.insert_element_for(tag_mut(token));
            }
            "optgroup" | "option" => {
                if self.current_element_is("option") {
                    self.process_end_tag("option");
                }
                self.reconstruct_formatting_elements();
                self.insert_element_for(tag_mut(token));
            }
            "rb" | "rtc" => {
                if self.in_scope("ruby") {
                    self.generate_implied_end_tags(false);
                }
                self.insert_element_for(tag_mut(token));
            }
            "rp" | "rt" => {
                if self.in_scope("ruby") {
                    self.generate_implied_end_tags_except("rtc");
                }
                self.insert_element_for(tag_mut(token));
            }
            "area" | "br" | "embed" | "img" | "keygen" | "wbr" => {
                self.reconstruct_formatting_elements();
                self.insert_empty_element_for(tag_mut(token));
                self.frameset_ok = false;
            }
            "b" | "big" | "code" | "em" | "font" | "i" | "s" | "small" | "strike" | "strong"
            | "tt" | "u" => {
                self.reconstruct_formatting_elements();
                let element = self.insert_element_for(tag_mut(token));
                self.push_active_formatting_elements(element);
            }
            _ => {
                let id = {
                    let tag = tag_mut(token);
                    let tag_name = string(tag.name());
                    let normal = tag.normal_name().to_string();
                    self.dom
                        .tags
                        .value_of(&tag_name, Some(&normal), Namespace::Html, false)
                };
                let tag = self.dom.tags.tag(id);
                let text_state = if tag.is(RCDATA) {
                    Some(State::Rcdata)
                } else if tag.is(DATA) {
                    Some(State::Rawtext)
                } else {
                    None
                };
                let known = tag.is_known();
                if text_state.is_some() {
                    self.handle_text_state(tag_mut(token), text_state);
                } else if !known {
                    self.insert_element_for(tag_mut(token));
                } else if IN_BODY_START_P_CLOSERS.contains(&name.as_str()) {
                    if self.in_button_scope("p") {
                        self.process_end_tag("p");
                    }
                    self.insert_element_for(tag_mut(token));
                } else if IN_BODY_START_TO_HEAD.contains(&name.as_str()) {
                    return self.process_in(token, Mode::InHead);
                } else if IN_BODY_START_APPLETS.contains(&name.as_str()) {
                    self.reconstruct_formatting_elements();
                    self.insert_element_for(tag_mut(token));
                    self.formatting.push(None);
                    self.frameset_ok = false;
                } else if IN_BODY_START_MEDIA.contains(&name.as_str()) {
                    self.insert_empty_element_for(tag_mut(token));
                } else if IN_BODY_START_DROP.contains(&name.as_str()) {
                    return false;
                } else {
                    self.reconstruct_formatting_elements();
                    self.insert_element_for(tag_mut(token));
                }
            }
        }
        true
    }

    fn in_body_end_tag(&mut self, token: &mut Token) -> bool {
        let name = tag_mut(token).normal_name().to_string();
        match name.as_str() {
            "template" => {
                self.process_in(token, Mode::InHead);
            }
            "sarcasm" | "span" => return self.any_other_end_tag(&name),
            "li" => {
                if !self.in_list_item_scope(&name) {
                    return false;
                }
                self.generate_implied_end_tags_except(&name);
                self.pop_stack_to_close(&name);
            }
            "body" => {
                if !self.in_scope("body") {
                    return false;
                }
                self.transition(Mode::AfterBody);
            }
            "html" => {
                if !self.on_stack("body") {
                    return false;
                }
                self.transition(Mode::AfterBody);
                return self.process(token);
            }
            "form" => {
                if !self.on_stack("template") {
                    let current_form = self.form.take();
                    let Some(current_form) = current_form else {
                        return false;
                    };
                    if !self.in_scope(&name) {
                        return false;
                    }
                    self.generate_implied_end_tags(false);
                    self.remove_from_stack(current_form);
                } else {
                    if !self.in_scope(&name) {
                        return false;
                    }
                    self.generate_implied_end_tags(false);
                    self.pop_stack_to_close(&name);
                }
            }
            "p" => {
                if !self.in_button_scope(&name) {
                    self.process_start_tag(&name);
                    return self.process(token);
                }
                self.generate_implied_end_tags_except(&name);
                self.pop_stack_to_close(&name);
            }
            "dd" | "dt" => {
                if !self.in_scope(&name) {
                    return false;
                }
                self.generate_implied_end_tags_except(&name);
                self.pop_stack_to_close(&name);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                if !self.has_heading_in_scope() {
                    return false;
                }
                self.generate_implied_end_tags_except(&name);
                self.pop_stack_to_close_any(HEADINGS);
            }
            "br" => {
                self.process_start_tag("br");
                return false;
            }
            _ => {
                if IN_BODY_END_ADOPTION_FORMATTERS.contains(&name.as_str()) {
                    return self.adoption_agency(&name);
                } else if IN_BODY_END_CLOSERS.contains(&name.as_str()) {
                    if !self.in_scope(&name) {
                        return false;
                    }
                    self.generate_implied_end_tags(false);
                    self.pop_stack_to_close(&name);
                } else if IN_BODY_START_APPLETS.contains(&name.as_str()) {
                    if !self.in_scope("name") {
                        if !self.in_scope(&name) {
                            return false;
                        }
                        self.generate_implied_end_tags(false);
                        self.pop_stack_to_close(&name);
                        self.clear_formatting_elements_to_last_marker();
                    }
                } else {
                    return self.any_other_end_tag(&name);
                }
            }
        }
        true
    }

    fn any_other_end_tag(&mut self, name: &str) -> bool {
        if self.get_from_stack(name).is_none() {
            return false;
        }
        for pos in (0..self.stack.len()).rev() {
            let node = self.stack[pos];
            if self.dom.name_is(node, name) {
                self.generate_implied_end_tags_except(name);
                self.pop_stack_to_close(name);
                break;
            }
            if self.is_special(node) {
                return false;
            }
        }
        true
    }

    fn adoption_agency(&mut self, subject: &str) -> bool {
        if self.current_element_is(subject)
            && !self.is_in_active_formatting_elements(self.current())
        {
            self.pop();
            return true;
        }
        let mut outer = 0;
        loop {
            if outer >= 8 {
                return true;
            }
            outer += 1;
            let mut format_element = None;
            for entry in self.formatting.iter().rev() {
                match entry {
                    None => break,
                    Some(element) if self.dom.normal_name(*element) == subject => {
                        format_element = Some(*element);
                        break;
                    }
                    Some(_) => {}
                }
            }
            let Some(format_element) = format_element else {
                return self.any_other_end_tag(subject);
            };
            if !self.on_stack_node(format_element) {
                self.remove_from_active_formatting_elements(format_element);
                return true;
            }
            if !self.in_scope(self.dom.normal_name(format_element)) {
                return false;
            }
            let mut furthest_block = None;
            if let Some(index) = self.stack.iter().rposition(|&open| open == format_element) {
                for &element in &self.stack[index + 1..] {
                    if self.is_special(element) {
                        furthest_block = Some(element);
                        break;
                    }
                }
            }
            let Some(furthest_block) = furthest_block else {
                while self.current() != format_element {
                    self.pop();
                }
                self.pop();
                self.remove_from_active_formatting_elements(format_element);
                return true;
            };
            let Some(common_ancestor) = self.above_on_stack(format_element) else {
                return true;
            };
            let mut bookmark = self.position_of_element(format_element);
            let mut node = furthest_block;
            let mut last_node = furthest_block;
            let mut inner = 0;
            loop {
                inner += 1;
                let next = if !self.on_stack_node(node) {
                    self.dom.parent(node).filter(|&parent| {
                        matches!(self.dom.node(parent).data, NodeData::Element { .. })
                    })
                } else {
                    self.above_on_stack(node)
                };
                let Some(next) = next else {
                    break;
                };
                node = next;
                if self.dom.name_is(node, "body") {
                    break;
                }
                if node == format_element {
                    break;
                }
                if inner > 3 && self.is_in_active_formatting_elements(node) {
                    self.remove_from_active_formatting_elements(node);
                    break;
                }
                if !self.is_in_active_formatting_elements(node) {
                    self.remove_from_stack(node);
                    continue;
                }
                if !self.on_stack_node(node) {
                    self.remove_from_active_formatting_elements(node);
                    break;
                }
                let replacement = self.recreate_element(node);
                self.replace_active_formatting_element(node, replacement);
                self.replace_on_stack(node, replacement);
                node = replacement;
                if last_node == furthest_block {
                    bookmark = Some(
                        self.position_of_element(node)
                            .map_or(0, |position| position + 1),
                    );
                }
                self.dom.append(node, last_node);
                last_node = node;
            }
            self.dom.append(common_ancestor, last_node);
            let adopter = self.recreate_element(format_element);
            for child in self.dom.children(furthest_block) {
                self.dom.append(adopter, child);
            }
            self.dom.append(furthest_block, adopter);
            self.remove_from_active_formatting_elements(format_element);
            self.push_with_bookmark(adopter, bookmark);
            self.remove_from_stack(format_element);
            self.insert_on_stack_after(furthest_block, adopter);
        }
    }

    fn text(&mut self, token: &mut Token) -> bool {
        match token {
            Token::Character { .. } => self.insert_character_node(token, false),
            Token::Eof => {
                self.pop();
                let original = self.original_mode();
                self.transition(original);
                if self.mode == Mode::Text {
                    self.transition(Mode::InBody);
                }
                return self.process(token);
            }
            Token::Tag(tag) if !tag.start => {
                self.pop();
                let original = self.original_mode();
                self.transition(original);
            }
            _ => {}
        }
        true
    }

    fn in_table(&mut self, token: &mut Token) -> bool {
        match token {
            Token::Character { .. }
                if self.stack.last().is_some_and(|&current| {
                    IN_TABLE_FOSTER.contains(&self.dom.normal_name(current))
                }) =>
            {
                self.pending_table_characters.clear();
                self.mark_insertion_mode();
                self.transition(Mode::InTableText);
                self.process(token)
            }
            Token::Comment(data) => {
                let data = data.clone();
                self.insert_comment(&data);
                true
            }
            Token::Doctype(_) => false,
            Token::Tag(tag) if tag.start => {
                let name = tag.normal_name().to_string();
                match name.as_str() {
                    "caption" => {
                        self.clear_stack_to_table_context();
                        self.formatting.push(None);
                        self.insert_element_for(tag);
                        self.transition(Mode::InCaption);
                    }
                    "colgroup" => {
                        self.clear_stack_to_table_context();
                        self.insert_element_for(tag);
                        self.transition(Mode::InColumnGroup);
                    }
                    "col" => {
                        self.clear_stack_to_table_context();
                        self.process_start_tag("colgroup");
                        return self.process(token);
                    }
                    _ if IN_TABLE_TO_BODY.contains(&name.as_str()) => {
                        self.clear_stack_to_table_context();
                        self.insert_element_for(tag);
                        self.transition(Mode::InTableBody);
                    }
                    _ if IN_TABLE_ADD_BODY.contains(&name.as_str()) => {
                        self.clear_stack_to_table_context();
                        self.process_start_tag("tbody");
                        return self.process(token);
                    }
                    "table" => {
                        if !self.in_table_scope(&name) {
                            return false;
                        }
                        self.pop_stack_to_close(&name);
                        if !self.reset_insertion_mode() {
                            self.insert_element_for(tag_mut(token));
                            return true;
                        }
                        return self.process(token);
                    }
                    _ if IN_TABLE_TO_HEAD.contains(&name.as_str()) => {
                        return self.process_in(token, Mode::InHead);
                    }
                    "noscript" => self.start_noscript(tag),
                    "input" => {
                        let hidden = tag.attributes.as_ref().is_some_and(|attributes| {
                            let value = attributes
                                .iter()
                                .find(|attribute| string(&attribute.name) == "type")
                                .and_then(|attribute| attribute.value.as_deref())
                                .unwrap_or(&[]);
                            equals_ignore_case(value, &"hidden".encode_utf16().collect::<Vec<_>>())
                        });
                        if !hidden {
                            return self.in_table_anything_else(token);
                        }
                        self.insert_empty_element_for(tag);
                    }
                    "form" => {
                        if self.form.is_some() || self.on_stack("template") {
                            return false;
                        }
                        self.insert_form_element(tag, false, false);
                    }
                    _ => return self.in_table_anything_else(token),
                }
                true
            }
            Token::Tag(tag) => {
                let name = tag.normal_name().to_string();
                if name == "table" {
                    if !self.in_table_scope(&name) {
                        return false;
                    }
                    self.pop_stack_to_close("table");
                    self.reset_insertion_mode();
                } else if IN_TABLE_END_ERR.contains(&name.as_str()) {
                    return false;
                } else if name == "template" {
                    self.process_in(token, Mode::InHead);
                } else {
                    return self.in_table_anything_else(token);
                }
                true
            }
            Token::Eof => true,
            Token::Character { .. } => self.in_table_anything_else(token),
        }
    }

    fn in_table_anything_else(&mut self, token: &mut Token) -> bool {
        self.foster_inserts = true;
        self.process_in(token, Mode::InBody);
        self.foster_inserts = false;
        true
    }

    fn in_table_text(&mut self, token: &mut Token) -> bool {
        if let Token::Character { data, .. } = token {
            self.pending_table_characters.push(data.clone());
            return true;
        }
        let pending = std::mem::take(&mut self.pending_table_characters);
        for data in pending {
            let mut character = Token::Character { data, cdata: false };
            if !whitespace(&character) {
                let current_fosters = self.stack.last().is_some_and(|&current| {
                    IN_TABLE_FOSTER.contains(&self.dom.normal_name(current))
                });
                if current_fosters {
                    self.foster_inserts = true;
                    self.process_in(&mut character, Mode::InBody);
                    self.foster_inserts = false;
                } else {
                    self.process_in(&mut character, Mode::InBody);
                }
            } else {
                self.insert_character_node(&mut character, false);
            }
        }
        let original = self.original_mode();
        self.transition(original);
        self.process(token)
    }

    fn in_caption(&mut self, token: &mut Token) -> bool {
        if token.is_end_named("caption") {
            if !self.in_table_scope("caption") {
                return false;
            }
            self.generate_implied_end_tags(false);
            self.pop_stack_to_close("caption");
            self.clear_formatting_elements_to_last_marker();
            self.transition(Mode::InTable);
            return true;
        }
        let closes = match token {
            Token::Tag(tag) if tag.start => IN_CELL_COL.contains(&tag.normal_name()),
            Token::Tag(tag) => tag.normal_name() == "table",
            _ => false,
        };
        if closes {
            if !self.in_table_scope("caption") {
                return false;
            }
            self.generate_implied_end_tags(false);
            self.pop_stack_to_close("caption");
            self.clear_formatting_elements_to_last_marker();
            self.transition(Mode::InTable);
            self.in_table(token);
            return true;
        }
        if matches!(token, Token::Tag(tag) if !tag.start && IN_CAPTION_IGNORE.contains(&tag.normal_name()))
        {
            return false;
        }
        self.process_in(token, Mode::InBody)
    }

    fn in_column_group(&mut self, token: &mut Token) -> bool {
        if whitespace(token) {
            self.insert_character_node(token, false);
            return true;
        }
        match token {
            Token::Comment(data) => {
                let data = data.clone();
                self.insert_comment(&data);
            }
            Token::Doctype(_) => {}
            Token::Tag(tag) if tag.start => match tag.normal_name() {
                "html" => return self.process_in(token, Mode::InBody),
                "col" => {
                    self.insert_empty_element_for(tag);
                }
                "template" => {
                    self.process_in(token, Mode::InHead);
                }
                _ => return self.in_column_group_anything_else(token),
            },
            Token::Tag(tag) => match tag.normal_name() {
                "colgroup" => {
                    if !self.current_element_is("colgroup") {
                        return false;
                    }
                    self.pop();
                    self.transition(Mode::InTable);
                }
                "template" => {
                    self.process_in(token, Mode::InHead);
                }
                _ => return self.in_column_group_anything_else(token),
            },
            Token::Eof => {
                if self.current_element_is("html") {
                    return true;
                }
                return self.in_column_group_anything_else(token);
            }
            Token::Character { .. } => return self.in_column_group_anything_else(token),
        }
        true
    }

    fn in_column_group_anything_else(&mut self, token: &mut Token) -> bool {
        if !self.current_element_is("colgroup") {
            return false;
        }
        self.pop();
        self.transition(Mode::InTable);
        self.process(token);
        true
    }

    fn in_table_body(&mut self, token: &mut Token) -> bool {
        match token {
            Token::Tag(tag) if tag.start => {
                let name = tag.normal_name().to_string();
                if name == "tr" {
                    self.clear_stack_to_table_body_context();
                    self.insert_element_for(tag);
                    self.transition(Mode::InRow);
                } else if IN_CELL_NAMES.contains(&name.as_str()) {
                    self.process_start_tag("tr");
                    return self.process(token);
                } else if IN_TABLE_BODY_EXIT.contains(&name.as_str()) {
                    return self.exit_table_body(token);
                } else {
                    return self.process_in(token, Mode::InTable);
                }
                true
            }
            Token::Tag(tag) => {
                let name = tag.normal_name().to_string();
                if IN_TABLE_END_IGNORE.contains(&name.as_str()) {
                    if !self.in_table_scope(&name) {
                        return false;
                    }
                    self.clear_stack_to_table_body_context();
                    self.pop();
                    self.transition(Mode::InTable);
                } else if name == "table" {
                    return self.exit_table_body(token);
                } else if IN_TABLE_BODY_END_IGNORE.contains(&name.as_str()) {
                    return false;
                } else {
                    return self.process_in(token, Mode::InTable);
                }
                true
            }
            _ => self.process_in(token, Mode::InTable),
        }
    }

    fn exit_table_body(&mut self, token: &mut Token) -> bool {
        if !(self.in_table_scope("tbody")
            || self.in_table_scope("thead")
            || self.in_table_scope("tfoot"))
        {
            return false;
        }
        self.clear_stack_to_table_body_context();
        let name = self.dom.normal_name(self.current()).to_string();
        self.process_end_tag(&name);
        self.process(token)
    }

    fn in_row(&mut self, token: &mut Token) -> bool {
        match token {
            Token::Tag(tag) if tag.start => {
                let name = tag.normal_name().to_string();
                if IN_CELL_NAMES.contains(&name.as_str()) {
                    self.clear_stack_to_table_row_context();
                    self.insert_element_for(tag);
                    self.transition(Mode::InCell);
                    self.formatting.push(None);
                } else if IN_ROW_MISSING.contains(&name.as_str()) {
                    if !self.in_table_scope("tr") {
                        return false;
                    }
                    self.clear_stack_to_table_row_context();
                    self.pop();
                    self.transition(Mode::InTableBody);
                    return self.process(token);
                } else {
                    return self.process_in(token, Mode::InTable);
                }
                true
            }
            Token::Tag(tag) => {
                let name = tag.normal_name().to_string();
                if name == "tr" {
                    if !self.in_table_scope(&name) {
                        return false;
                    }
                    self.clear_stack_to_table_row_context();
                    self.pop();
                    self.transition(Mode::InTableBody);
                } else if name == "table" {
                    if !self.in_table_scope("tr") {
                        return false;
                    }
                    self.clear_stack_to_table_row_context();
                    self.pop();
                    self.transition(Mode::InTableBody);
                    return self.process(token);
                } else if IN_TABLE_TO_BODY.contains(&name.as_str()) {
                    if !self.in_table_scope(&name) {
                        return false;
                    }
                    if !self.in_table_scope("tr") {
                        return false;
                    }
                    self.clear_stack_to_table_row_context();
                    self.pop();
                    self.transition(Mode::InTableBody);
                    return self.process(token);
                } else if IN_ROW_IGNORE.contains(&name.as_str()) {
                    return false;
                } else {
                    return self.process_in(token, Mode::InTable);
                }
                true
            }
            _ => self.process_in(token, Mode::InTable),
        }
    }

    fn in_cell(&mut self, token: &mut Token) -> bool {
        match token {
            Token::Tag(tag) if !tag.start => {
                let name = tag.normal_name().to_string();
                if IN_CELL_NAMES.contains(&name.as_str()) {
                    if !self.in_table_scope(&name) {
                        self.transition(Mode::InRow);
                        return false;
                    }
                    self.generate_implied_end_tags(false);
                    self.pop_stack_to_close(&name);
                    self.clear_formatting_elements_to_last_marker();
                    self.transition(Mode::InRow);
                } else if IN_CELL_BODY.contains(&name.as_str()) {
                    return false;
                } else if IN_CELL_TABLE.contains(&name.as_str()) {
                    if !self.in_table_scope(&name) {
                        return false;
                    }
                    self.close_cell();
                    return self.process(token);
                } else {
                    return self.process_in(token, Mode::InBody);
                }
                true
            }
            Token::Tag(tag) if IN_CELL_COL.contains(&tag.normal_name()) => {
                if !(self.in_table_scope("td") || self.in_table_scope("th")) {
                    return false;
                }
                self.close_cell();
                self.process(token)
            }
            _ => self.process_in(token, Mode::InBody),
        }
    }

    fn close_cell(&mut self) {
        if self.in_table_scope("td") {
            self.process_end_tag("td");
        } else {
            self.process_end_tag("th");
        }
    }

    fn in_select(&mut self, token: &mut Token) -> bool {
        match token {
            Token::Character { .. } => {
                self.insert_character_node(token, false);
                true
            }
            Token::Comment(data) => {
                let data = data.clone();
                self.insert_comment(&data);
                true
            }
            Token::Doctype(_) => false,
            Token::Tag(tag) if tag.start => {
                let name = tag.normal_name().to_string();
                if name == "html" {
                    return self.process_in(token, Mode::InBody);
                } else if name == "option" {
                    if self.current_element_is("option") {
                        self.process_end_tag("option");
                    }
                    self.insert_element_for(tag_mut(token));
                } else if name == "optgroup" {
                    if self.current_element_is("option") {
                        self.process_end_tag("option");
                    }
                    if self.current_element_is("optgroup") {
                        self.process_end_tag("optgroup");
                    }
                    self.insert_element_for(tag_mut(token));
                } else if name == "select" {
                    return self.process_end_tag("select");
                } else if IN_SELECT_END.contains(&name.as_str()) {
                    if !self.in_select_scope("select") {
                        return false;
                    }
                    loop {
                        self.pop_stack_to_close("select");
                        self.reset_insertion_mode();
                        if !self.in_select_scope("select") {
                            break;
                        }
                    }
                    return self.process(token);
                } else if name == "script" || name == "template" {
                    return self.process_in(token, Mode::InHead);
                } else if name == "noscript" {
                    self.start_noscript(tag_mut(token));
                } else {
                    return false;
                }
                true
            }
            Token::Tag(tag) => {
                let name = tag.normal_name().to_string();
                match name.as_str() {
                    "optgroup" => {
                        if self.current_element_is("option") {
                            let above = self.above_on_stack(self.current());
                            if above.is_some_and(|above| self.dom.name_is(above, "optgroup")) {
                                self.process_end_tag("option");
                            }
                        }
                        if self.current_element_is("optgroup") {
                            self.pop();
                        }
                    }
                    "option" => {
                        if self.current_element_is("option") {
                            self.pop();
                        }
                    }
                    "select" => {
                        if !self.in_select_scope(&name) {
                            return false;
                        }
                        self.pop_stack_to_close(&name);
                        self.reset_insertion_mode();
                    }
                    "template" => return self.process_in(token, Mode::InHead),
                    _ => return false,
                }
                true
            }
            Token::Eof => true,
        }
    }

    fn in_select_in_table(&mut self, token: &mut Token) -> bool {
        match token {
            Token::Tag(tag) if tag.start && IN_SELECT_TABLE_END.contains(&tag.normal_name()) => {
                self.pop_stack_to_close("select");
                self.reset_insertion_mode();
                self.process(token)
            }
            Token::Tag(tag) if !tag.start && IN_SELECT_TABLE_END.contains(&tag.normal_name()) => {
                let name = tag.normal_name().to_string();
                if self.in_table_scope(&name) {
                    self.pop_stack_to_close("select");
                    self.reset_insertion_mode();
                    self.process(token)
                } else {
                    false
                }
            }
            _ => self.process_in(token, Mode::InSelect),
        }
    }

    fn in_template(&mut self, token: &mut Token) -> bool {
        match token {
            Token::Character { .. } | Token::Comment(_) | Token::Doctype(_) => {
                self.process_in(token, Mode::InBody);
            }
            Token::Tag(tag) if tag.start => {
                let name = tag.normal_name().to_string();
                if IN_TEMPLATE_TO_HEAD.contains(&name.as_str()) {
                    self.process_in(token, Mode::InHead);
                } else {
                    let mode = if IN_TEMPLATE_TO_TABLE.contains(&name.as_str()) {
                        Mode::InTable
                    } else if name == "col" {
                        Mode::InColumnGroup
                    } else if name == "tr" {
                        Mode::InTableBody
                    } else if name == "td" || name == "th" {
                        Mode::InRow
                    } else {
                        Mode::InBody
                    };
                    self.template_modes.pop();
                    self.template_modes.push(mode);
                    self.transition(mode);
                    return self.process(token);
                }
            }
            Token::Tag(tag) => {
                if tag.normal_name() == "template" {
                    self.process_in(token, Mode::InHead);
                } else {
                    return false;
                }
            }
            Token::Eof => {
                if !self.on_stack("template") {
                    return true;
                }
                self.pop_stack_to_close("template");
                self.clear_formatting_elements_to_last_marker();
                self.template_modes.pop();
                self.reset_insertion_mode();
                if self.mode != Mode::InTemplate && self.template_modes.len() < 12 {
                    return self.process(token);
                }
                return true;
            }
        }
        true
    }

    fn after_body(&mut self, token: &mut Token) -> bool {
        let html = self.get_from_stack("html");
        if whitespace(token) {
            match html {
                Some(html) => self.insert_character_to_element(token, html),
                None => {
                    self.process_in(token, Mode::InBody);
                }
            }
            return true;
        }
        match token {
            Token::Comment(data) => {
                let data = data.clone();
                match html {
                    Some(html) => self.insert_comment_node(&data, html),
                    None => self.insert_comment(&data),
                }
                true
            }
            Token::Doctype(_) => false,
            Token::Tag(tag) if tag.start && tag.normal_name() == "html" => {
                self.process_in(token, Mode::InBody)
            }
            Token::Tag(tag) if !tag.start && tag.normal_name() == "html" => {
                self.transition(Mode::AfterAfterBody);
                true
            }
            Token::Eof => true,
            _ => {
                self.reset_body();
                self.process(token)
            }
        }
    }

    fn in_frameset(&mut self, token: &mut Token) -> bool {
        if whitespace(token) {
            self.insert_character_node(token, false);
            return true;
        }
        match token {
            Token::Comment(data) => {
                let data = data.clone();
                self.insert_comment(&data);
                true
            }
            Token::Doctype(_) => false,
            Token::Tag(tag) if tag.start => match tag.normal_name() {
                "html" => self.process_in(token, Mode::InBody),
                "frameset" => {
                    self.insert_element_for(tag);
                    true
                }
                "frame" => {
                    self.insert_empty_element_for(tag);
                    true
                }
                "noframes" => self.process_in(token, Mode::InHead),
                _ => false,
            },
            Token::Tag(tag) if tag.normal_name() == "frameset" => {
                if !self.current_element_is("frameset") {
                    return false;
                }
                self.pop();
                if !self.current_element_is("frameset") {
                    self.transition(Mode::AfterFrameset);
                }
                true
            }
            Token::Eof => true,
            _ => false,
        }
    }

    fn after_frameset(&mut self, token: &mut Token) -> bool {
        if whitespace(token) {
            self.insert_character_node(token, false);
            return true;
        }
        match token {
            Token::Comment(data) => {
                let data = data.clone();
                self.insert_comment(&data);
                true
            }
            Token::Doctype(_) => false,
            Token::Tag(tag) if tag.start && tag.normal_name() == "html" => {
                self.process_in(token, Mode::InBody)
            }
            Token::Tag(tag) if !tag.start && tag.normal_name() == "html" => {
                self.transition(Mode::AfterAfterFrameset);
                true
            }
            Token::Tag(tag) if tag.start && tag.normal_name() == "noframes" => {
                self.process_in(token, Mode::InHead)
            }
            Token::Eof => true,
            _ => false,
        }
    }

    fn after_after_body(&mut self, token: &mut Token) -> bool {
        match token {
            Token::Comment(data) => {
                let data = data.clone();
                self.insert_comment_node(&data, DOCUMENT);
                true
            }
            Token::Doctype(_) => self.process_in(token, Mode::InBody),
            Token::Tag(tag) if tag.start && tag.normal_name() == "html" => {
                self.process_in(token, Mode::InBody)
            }
            _ if whitespace(token) => {
                self.insert_character_to_element(token, DOCUMENT);
                true
            }
            Token::Eof => true,
            _ => {
                self.reset_body();
                self.process(token)
            }
        }
    }

    fn after_after_frameset(&mut self, token: &mut Token) -> bool {
        match token {
            Token::Comment(data) => {
                let data = data.clone();
                self.insert_comment_node(&data, DOCUMENT);
                true
            }
            Token::Doctype(_) => self.process_in(token, Mode::InBody),
            _ if whitespace(token) => self.process_in(token, Mode::InBody),
            Token::Tag(tag) if tag.start && tag.normal_name() == "html" => {
                self.process_in(token, Mode::InBody)
            }
            Token::Eof => true,
            Token::Tag(tag) if tag.start && tag.normal_name() == "noframes" => {
                self.process_in(token, Mode::InHead)
            }
            _ => false,
        }
    }

    fn foreign_content(&mut self, token: &mut Token) -> bool {
        match token {
            Token::Character { .. } => {
                if whitespace(token) {
                    self.insert_character_node(token, false);
                } else {
                    self.insert_character_node(token, true);
                    self.frameset_ok = false;
                }
                true
            }
            Token::Comment(data) => {
                let data = data.clone();
                self.insert_comment(&data);
                true
            }
            Token::Doctype(_) => true,
            Token::Tag(tag) if tag.start => {
                if IN_FOREIGN_TO_HTML.contains(&tag.normal_name()) {
                    return self.breakout_to_html(token);
                }
                if tag.normal_name() == "font"
                    && (tag.has_attribute_ignore_case("color")
                        || tag.has_attribute_ignore_case("face")
                        || tag.has_attribute_ignore_case("size"))
                {
                    return self.breakout_to_html(token);
                }
                let namespace = self.dom.namespace(self.current());
                let tag = tag_mut(token);
                self.insert_foreign_element_for(tag, namespace);
                let name = string(tag.name());
                let id = self
                    .dom
                    .tags
                    .value_of(&name, Some(tag.normal_name()), namespace, false);
                let looked_up = self.dom.tags.tag(id);
                let text_state = if looked_up.is(RCDATA) {
                    Some(State::Rcdata)
                } else if looked_up.is(DATA) {
                    Some(State::Rawtext)
                } else {
                    None
                };
                if text_state.is_some() {
                    if tag.normal_name() == "script" {
                        self.tokeniser.transition(State::ScriptData);
                    } else if let Some(state) = text_state {
                        self.tokeniser.transition(state);
                    }
                }
                true
            }
            Token::Tag(tag) => {
                let name = tag.normal_name().to_string();
                if name == "br" || name == "p" {
                    return self.breakout_to_html(token);
                }
                if name == "script" && self.current_element_is_in("script", Namespace::Svg) {
                    self.pop();
                    return true;
                }
                let mut i = self.stack.len() - 1;
                let mut element = self.stack[i];
                while i != 0 {
                    if self.dom.name_is(element, &name) {
                        let normal = self.dom.normal_name(element).to_string();
                        self.pop_stack_to_close_any_namespace(&normal);
                        return true;
                    }
                    i -= 1;
                    element = self.stack[i];
                    if self.dom.namespace(element) == Namespace::Html {
                        return self.process_in(token, self.mode);
                    }
                }
                true
            }
            Token::Eof => true,
        }
    }

    fn breakout_to_html(&mut self, token: &mut Token) -> bool {
        while let Some(&current) = self.stack.last() {
            if self.dom.namespace(current) == Namespace::Html
                || self.is_mathml_text_integration(current)
                || self.is_html_integration(current)
            {
                break;
            }
            self.pop();
        }
        self.process_in(token, self.mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_trees_as_jsoup_does() {
        let text: Vec<u16> = "<p>one<p>two<div/>after".encode_utf16().collect();
        let dom = parse(&text, Source::String);
        let html = dom.node(DOCUMENT).children[0];
        let body = dom.node(html).children[1];
        let children: Vec<String> = dom
            .node(body)
            .children
            .iter()
            .map(|&child| match &dom.node(child).data {
                NodeData::Text(text) => format!("#{text}"),
                _ => format!(
                    "{}:{}",
                    dom.normal_name(child),
                    dom.node(child).children.len()
                ),
            })
            .collect();
        assert_eq!(children, ["p:1", "p:1", "div:0", "#after"]);
    }
}
