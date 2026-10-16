//! Tika's `PDFMarkedContent2XHTML`: a tagged document's pages written through its structure
//! tree. The stripper's words and separators of a page are recorded, each word labelled with the
//! marked content its glyphs were drawn under; a page the tree describes well enough is written
//! block by block in tree order, others as `PDF2XHTML` writes them, and text the tree does not
//! own follows in `untagged` and `artifact` divisions.

use std::collections::{HashMap, HashSet};

use super::cos::{text_string, Pdf};
use super::engine::PageContent;
use super::jfloat;
use super::position::TextPosition;
use super::stripper::{self, WORD_SEPARATOR};
use super::structure::{
    self, attribute_int, attributes, is_standard_type, Label, Leaf, StructureIndex,
};
use super::writer::{PageWriter, WriteFailure};
use crate::formats::xhtml::Stop;
use crate::java::is_blank;

/// `PDF.MARKED_CONTENT_REJECTIONS`.
pub(super) const MARKED_CONTENT_REJECTIONS: &str = "pdf:marked-content-rejections";
const MAX_CLASS_NAME: usize = 64;
const MAX_OPEN_ELEMENTS: usize = 60;
const MIN_WORDS_FOR_SHREDDING: usize = 10;
const MAX_SHREDDED_RATIO: f32 = 0.3;
const MIN_COVERAGE: f32 = 0.5;
const MAX_DANGLING_RATIO: f32 = 0.2;
const GROUPING_TYPES: [&str; 9] = [
    "Part",
    "Art",
    "Sect",
    "Div",
    "Aside",
    "DocumentFragment",
    "Private",
    "NonStruct",
    "Index",
];
const STRUCTURAL_TAGS: [&str; 11] = [
    "table", "thead", "tbody", "tfoot", "tr", "td", "th", "caption", "ul", "ol", "li",
];
const TABLE_TAGS: [&str; 5] = ["table", "thead", "tbody", "tfoot", "tr"];
const ORDERED_LIST_NUMBERING: [&str; 5] = [
    "Decimal",
    "LowerAlpha",
    "UpperAlpha",
    "LowerRoman",
    "UpperRoman",
];

/// What the writer is doing with the stripper's calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Routing,
    Flat,
    Tagged,
}

/// A word's glyphs under one label.
#[derive(Debug, Clone)]
struct Segment {
    label: Label,
    text: String,
    positions: usize,
}

/// A recorded call of the stripper.
#[derive(Debug, Clone)]
enum Event {
    ParagraphStart,
    ParagraphEnd,
    Word {
        text: String,
        segments: Vec<Segment>,
        words: usize,
        shredded: usize,
    },
    WordSeparator,
    LineSeparator,
}

/// A piece of a run: text, or a line or paragraph break.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Text(String),
    Newline,
    ParagraphBreak,
}

/// Consecutive words under one label, with the separators between them.
#[derive(Debug)]
struct Run {
    label: Label,
    ordinal: usize,
    pieces: Vec<Piece>,
    has_text: bool,
}

impl Run {
    fn ends_with_separator(&self) -> bool {
        match self.pieces.last() {
            None => false,
            Some(Piece::Newline | Piece::ParagraphBreak) => true,
            Some(Piece::Text(text)) => is_blank(text),
        }
    }

    fn has_whitespace(&self) -> bool {
        self.pieces.iter().any(|piece| match piece {
            Piece::Newline | Piece::ParagraphBreak => true,
            Piece::Text(text) => text.chars().any(crate::unicode::is_whitespace),
        })
    }
}

/// `ElementSpec`: how a structure element is written.
#[derive(Debug, Clone)]
pub(super) struct ElementSpec {
    tag: Option<String>,
    attributes: Vec<(String, String)>,
    inline: bool,
    alt: Option<String>,
    outer: Option<&'static str>,
    demoted: bool,
}

impl ElementSpec {
    fn transparent() -> Self {
        Self {
            tag: None,
            attributes: Vec::new(),
            inline: false,
            alt: None,
            outer: None,
            demoted: false,
        }
    }

    fn new(tag: &str, attributes: Vec<(String, String)>, inline: bool) -> Self {
        Self {
            tag: Some(tag.to_owned()),
            attributes,
            inline,
            alt: None,
            outer: None,
            demoted: false,
        }
    }

    fn as_span(&self) -> Self {
        let mut attributes = self.attributes.clone();
        if !attributes.iter().any(|(name, _)| name == "class") {
            attributes.push(("class".to_owned(), self.tag.clone().unwrap_or_default()));
        }
        Self {
            tag: Some("span".to_owned()),
            attributes,
            inline: true,
            alt: self.alt.clone(),
            outer: self.outer,
            demoted: true,
        }
    }

    fn within(&self, outer: &'static str) -> Self {
        Self {
            outer: Some(outer),
            ..self.clone()
        }
    }

    fn elements(&self) -> usize {
        match (&self.tag, self.outer) {
            (None, _) => 0,
            (Some(_), None) => 1,
            (Some(_), Some(_)) => 2,
        }
    }

    fn tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }
}

/// `Wrapper`: the element loose text is wrapped in.
#[derive(Debug)]
struct Wrapper {
    tag: &'static str,
    inside: bool,
    at: usize,
    open: bool,
    break_pending: bool,
}

/// The open state of a node on the open path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElementState {
    NotWritten,
    Open,
    Suspended,
    Dropped,
}

/// A block-level element's runs, in content order; `None` stands for an object leaf.
#[derive(Debug)]
struct BlockGroup {
    block: usize,
    loose: bool,
    runs: Vec<Option<usize>>,
    leaves: Vec<Leaf>,
    first_leaf_order: usize,
    has_text: bool,
    has_object: bool,
}

/// The tagged writer's state.
pub(super) struct Tagged<'p> {
    pub(super) reading_order: bool,
    index: Option<StructureIndex<'p, ElementSpec>>,
    pdf: &'p Pdf,
    state: State,
    events: Vec<Event>,
    pending_page_start: bool,
    page_tagged: bool,
    pub(super) pages_tagged: i32,
    pub(super) pages_fallback: i32,
    /// Why the document's tree is not used, if it is not.
    pub(super) document_rejection: Option<&'static str>,
    held: Vec<Piece>,
    at_block_start: bool,
    open: Vec<usize>,
    open_state: Vec<ElementState>,
    alt_written: HashSet<usize>,
}

impl<'p> Tagged<'p> {
    pub(super) fn new(pdf: &'p Pdf, reading_order: bool) -> Self {
        let index = StructureIndex::load(pdf);
        let document_rejection = index.reason;
        let index = document_rejection.is_none().then_some(index);
        Self {
            reading_order,
            index,
            pdf,
            state: State::Idle,
            events: Vec::new(),
            pending_page_start: false,
            page_tagged: false,
            pages_tagged: 0,
            pages_fallback: 0,
            document_rejection,
            held: Vec::new(),
            at_block_start: false,
            open: Vec::new(),
            open_state: Vec::new(),
            alt_written: HashSet::new(),
        }
    }

    pub(super) fn has_index(&self) -> bool {
        self.index.is_some()
    }

    fn of<'a>(writer: &'a mut PageWriter<'p, '_, '_>) -> &'a mut Tagged<'p> {
        writer
            .tagged
            .as_mut()
            .unwrap_or_else(|| unreachable!("the tagged writer's calls come with its state"))
    }

    /// `startPage`: the page's paragraph start held until the page is known to be tagged or
    /// not.
    pub(super) fn start_page(&mut self) {
        self.events.clear();
        self.page_tagged = false;
        self.pending_page_start = true;
    }

    pub(super) fn write_paragraph_start(writer: &mut PageWriter<'p, '_, '_>) -> Result<(), Stop> {
        let tagged = Self::of(writer);
        if tagged.state == State::Routing {
            tagged.events.push(Event::ParagraphStart);
            Ok(())
        } else if tagged.state != State::Tagged {
            writer.plain_paragraph_start()
        } else {
            Ok(())
        }
    }

    pub(super) fn write_paragraph_end(writer: &mut PageWriter<'p, '_, '_>) -> Result<(), Stop> {
        let tagged = Self::of(writer);
        if tagged.state == State::Routing {
            tagged.events.push(Event::ParagraphEnd);
            Ok(())
        } else if tagged.state == State::Idle && tagged.page_tagged {
            Ok(())
        } else if tagged.state != State::Tagged {
            Self::flush_pending_page_start(writer)?;
            writer.plain_paragraph_end()
        } else {
            Ok(())
        }
    }

    pub(super) fn write_string(
        writer: &mut PageWriter<'p, '_, '_>,
        text: &str,
        positions: &[&TextPosition],
    ) -> Result<(), Stop> {
        let tagged = Self::of(writer);
        if tagged.state != State::Routing {
            return writer.plain_string(text);
        }
        let event = tagged.word(text, positions);
        tagged.events.push(event);
        Ok(())
    }

    pub(super) fn write_word_separator(writer: &mut PageWriter<'p, '_, '_>) -> Result<(), Stop> {
        let tagged = Self::of(writer);
        if tagged.state == State::Routing {
            tagged.events.push(Event::WordSeparator);
            Ok(())
        } else {
            writer.plain_word_separator()
        }
    }

    pub(super) fn write_line_separator(writer: &mut PageWriter<'p, '_, '_>) -> Result<(), Stop> {
        let tagged = Self::of(writer);
        if tagged.state == State::Routing {
            tagged.events.push(Event::LineSeparator);
            Ok(())
        } else {
            writer.plain_line_separator()
        }
    }

    fn flush_pending_page_start(writer: &mut PageWriter<'p, '_, '_>) -> Result<(), Stop> {
        let tagged = Self::of(writer);
        if tagged.pending_page_start {
            tagged.pending_page_start = false;
            writer.plain_paragraph_start()?;
        }
        Ok(())
    }

    /// `writePage`: the page recorded, then written through the tree or flat.
    pub(super) fn write_page(
        writer: &mut PageWriter<'p, '_, '_>,
        page_number: usize,
        content: &mut PageContent,
    ) -> Result<(), WriteFailure> {
        if !Self::of(writer).has_index() {
            Self::of(writer).state = State::Flat;
            let result = Self::flush_pending_page_start(writer)
                .map_err(WriteFailure::from)
                .and_then(|()| {
                    stripper::write_page(&mut content.articles, writer).map_err(WriteFailure::from)
                });
            Self::of(writer).state = State::Idle;
            return result;
        }
        Self::of(writer).state = State::Routing;
        let routed = stripper::write_page(&mut content.articles, writer);
        Self::of(writer).state = State::Idle;
        routed?;
        let tagged = Self::of(writer);
        let rejection = tagged.gate(content);
        match rejection {
            None => {
                tagged.pages_tagged += 1;
                tagged.page_tagged = true;
                tagged.pending_page_start = false;
                tagged.state = State::Tagged;
                let result = Self::emit_tagged(writer, content);
                let tagged = Self::of(writer);
                tagged.state = State::Idle;
                tagged.events.clear();
                result.map_err(WriteFailure::from)
            }
            Some(rejection) => {
                tagged.pages_fallback += 1;
                tagged.state = State::Flat;
                writer.xhtml.metadata_mut().add(
                    MARKED_CONTENT_REJECTIONS,
                    format!("{page_number}:{rejection}"),
                );
                let result =
                    Self::flush_pending_page_start(writer).and_then(|()| Self::replay_flat(writer));
                let tagged = Self::of(writer);
                tagged.state = State::Idle;
                tagged.events.clear();
                result.map_err(WriteFailure::from)
            }
        }
    }

    /// `replayFlat`: the recorded calls, as `PDF2XHTML` would have made them.
    fn replay_flat(writer: &mut PageWriter<'p, '_, '_>) -> Result<(), Stop> {
        let events = std::mem::take(&mut Self::of(writer).events);
        for event in &events {
            match event {
                Event::ParagraphStart => writer.plain_paragraph_start()?,
                Event::ParagraphEnd => writer.plain_paragraph_end()?,
                Event::Word { text, .. } => writer.plain_string(text)?,
                Event::WordSeparator => writer.plain_word_separator()?,
                Event::LineSeparator => writer.plain_line_separator()?,
            }
        }
        Ok(())
    }

    /// The recorded word of `writeString` while routing.
    fn word(&self, text: &str, positions: &[&TextPosition]) -> Event {
        let label_of = |position: &TextPosition| {
            if self.index.is_some() {
                position.label
            } else {
                Label::Untagged
            }
        };
        let count = positions.len();
        let first = positions.first().map_or(Label::Untagged, |p| label_of(p));
        let straddled = positions.iter().skip(1).any(|p| label_of(p) != first);
        let mut segments = Vec::with_capacity(1);
        if !straddled {
            segments.push(Segment {
                label: first,
                text: text.to_owned(),
                positions: count,
            });
        } else if text != raw_text(positions) {
            segments.push(Segment {
                label: majority_label(positions, label_of),
                text: text.to_owned(),
                positions: count,
            });
        } else {
            let mut label = first;
            let mut part = String::new();
            let mut glyphs = 0;
            for position in positions {
                let current = label_of(position);
                if current != label {
                    segments.push(Segment {
                        label,
                        text: std::mem::take(&mut part),
                        positions: glyphs,
                    });
                    label = current;
                    glyphs = 0;
                }
                part.push_str(&position.unicode);
                glyphs += 1;
            }
            segments.push(Segment {
                label,
                text: part,
                positions: glyphs,
            });
        }
        let (words, shredded) = self.count_shredding(positions);
        Event::Word {
            text: text.to_owned(),
            segments,
            words,
            shredded,
        }
    }

    /// `countShredding`: the words of three glyphs or more, and those of them whose glyphs the
    /// tree spreads over three blocks or more.
    fn count_shredding(&self, positions: &[&TextPosition]) -> (usize, usize) {
        let Some(index) = &self.index else {
            return (0, 0);
        };
        let mut words = 0;
        let mut shredded = 0;
        let mut glyphs = 0;
        let mut nodes: HashSet<usize> = HashSet::new();
        for index_in_word in 0..=positions.len() {
            let position = positions.get(index_in_word);
            let boundary = position.is_none_or(|p| is_blank(&p.unicode));
            if boundary {
                if glyphs >= 3 {
                    words += 1;
                    if nodes.len() >= 3 {
                        shredded += 1;
                    }
                }
                glyphs = 0;
                nodes.clear();
                continue;
            }
            glyphs += 1;
            if let Some(Label::Tagged { scope, mcid }) = position.map(|p| p.label) {
                if let Some(leaf) = index.leaf(scope, mcid) {
                    nodes.insert(self.block_of(leaf.node));
                }
            }
        }
        (words, shredded)
    }

    /// The leaf a label stands for, if the tree owns it.
    fn leaf(&self, label: Label) -> Option<Leaf> {
        match label {
            Label::Tagged { scope, mcid } => self.index.as_ref()?.leaf(scope, mcid),
            _ => None,
        }
    }

    /// `gate`: why the page is not written through the tree, if it is not.
    fn gate(&self, content: &PageContent) -> Option<String> {
        let index = self.index.as_ref()?;
        let mut tagged = 0usize;
        let mut in_block = 0usize;
        let mut untagged = 0usize;
        let mut artifact = 0usize;
        let mut words = 0usize;
        let mut shredded = 0usize;
        for event in &self.events {
            let Event::Word {
                segments,
                words: event_words,
                shredded: event_shredded,
                ..
            } = event
            else {
                continue;
            };
            words += event_words;
            shredded += event_shredded;
            for segment in segments {
                if segment.label == Label::Artifact {
                    artifact += segment.positions;
                } else if let Some(leaf) = self.leaf(segment.label) {
                    tagged += segment.positions;
                    if self.wrapper_tag(self.block_of(leaf.node)).is_none() {
                        in_block += segment.positions;
                    }
                } else {
                    untagged += segment.positions;
                }
            }
        }
        let mut leaves = 0usize;
        let mut dangling = 0usize;
        for scope in &content.scopes {
            if let Some(scope_leaves) = index.leaves(*scope) {
                for mcid in scope_leaves.keys() {
                    leaves += 1;
                    if !content.opened.contains(&Label::Tagged {
                        scope: *scope,
                        mcid: *mcid,
                    }) {
                        dangling += 1;
                    }
                }
            }
        }
        if tagged == 0 {
            return Some("no-tagged-text".to_owned());
        }
        if in_block == 0 {
            return Some("no-block-structure".to_owned());
        }
        if words >= MIN_WORDS_FOR_SHREDDING && (shredded as f32) > words as f32 * MAX_SHREDDED_RATIO
        {
            let ratio = shredded as f32 / words as f32;
            return Some(format!("shredded={}", jfloat::format_fixed(ratio, 2)));
        }
        let coverage = tagged as f32 / (tagged + untagged + artifact) as f32;
        if coverage < MIN_COVERAGE {
            return Some(format!("coverage={}", jfloat::format_fixed(coverage, 2)));
        }
        if leaves > 0 {
            let ratio = dangling as f32 / leaves as f32;
            if ratio > MAX_DANGLING_RATIO {
                return Some(format!("dangling={}", jfloat::format_fixed(ratio, 2)));
            }
        }
        None
    }
}

/// `rawText`: the glyphs' text, joined.
fn raw_text(positions: &[&TextPosition]) -> String {
    positions.iter().map(|p| p.unicode.as_str()).collect()
}

/// `majorityLabel`: the label the most glyphs are drawn under, the first to lead on a tie.
fn majority_label(positions: &[&TextPosition], label_of: impl Fn(&TextPosition) -> Label) -> Label {
    let mut counts: HashMap<Label, usize> = HashMap::new();
    let mut best = Label::Untagged;
    let mut best_count = 0;
    for position in positions {
        let label = label_of(position);
        let count = counts.entry(label).or_default();
        *count += 1;
        if *count > best_count {
            best_count = *count;
            best = label;
        }
    }
    best
}

/// `className`: a structure type made a class name.
fn class_name(structure_type: &str) -> String {
    let mut name: String = structure_type
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    name.truncate(MAX_CLASS_NAME);
    if name.is_empty() {
        "unknown".to_owned()
    } else {
        name
    }
}

/// `addAttribute`: a non-blank value, unless the name is set.
fn add_attribute(attributes: &mut Vec<(String, String)>, name: &str, value: Option<String>) {
    let Some(value) = value else {
        return;
    };
    if is_blank(&value) || attributes.iter().any(|(existing, _)| existing == name) {
        return;
    }
    attributes.push((name.to_owned(), value));
}

impl<'p> Tagged<'p> {
    fn index(&self) -> &StructureIndex<'p, ElementSpec> {
        self.index
            .as_ref()
            .unwrap_or_else(|| unreachable!("tagged emission has an index"))
    }

    /// `spec`: a node's spec, built with its ancestors' from the top down.
    fn spec(&self, node: usize) -> &ElementSpec {
        let index = self.index();
        if let Some(spec) = index.nodes[node].spec.get() {
            return spec;
        }
        let mut chain = Vec::new();
        let mut current = Some(node);
        while let Some(n) = current {
            if index.nodes[n].spec.get().is_some() {
                break;
            }
            chain.push(n);
            current = index.nodes[n].parent;
        }
        for &n in chain.iter().rev() {
            let spec = self.build_spec(n);
            let _ = index.nodes[n].spec.set(spec);
        }
        index.nodes[node]
            .spec
            .get()
            .unwrap_or_else(|| unreachable!("the spec was just built"))
    }

    fn written_ancestor(&self, node: usize) -> Option<usize> {
        let index = self.index();
        let mut current = index.nodes[node].parent;
        while let Some(parent) = current {
            if index.nodes[parent]
                .spec
                .get()
                .is_some_and(|spec| spec.tag.is_some())
            {
                return Some(parent);
            }
            current = index.nodes[parent].parent;
        }
        None
    }

    fn ancestor_spec(&self, node: usize) -> Option<&ElementSpec> {
        self.written_ancestor(node)
            .and_then(|ancestor| self.index().nodes[ancestor].spec.get())
    }

    fn inline_context(&self, node: usize) -> bool {
        self.ancestor_spec(node).is_some_and(|spec| spec.inline)
    }

    fn table_context(&self, node: usize) -> bool {
        self.ancestor_spec(node)
            .and_then(ElementSpec::tag)
            .is_some_and(|tag| TABLE_TAGS.contains(&tag))
    }

    /// `buildSpec`.
    fn build_spec(&self, node: usize) -> ElementSpec {
        let mut spec = self.map_type(node);
        let Some(tag) = spec.tag.clone() else {
            return spec;
        };
        if !spec.inline
            && !STRUCTURAL_TAGS.contains(&tag.as_str())
            && (self.inline_context(node) || self.table_context(node))
        {
            spec = spec.as_span();
        }
        let container = self.ancestor_spec(node).and_then(ElementSpec::tag);
        let in_list = matches!(container, Some("ul" | "ol"));
        let in_table = container.is_some_and(|tag| TABLE_TAGS.contains(&tag) && tag != "tr");
        let tag = spec.tag.clone().unwrap_or_default();
        if in_list && tag != "li" {
            spec = spec.within("li");
        } else if tag == "li" && !in_list {
            spec = spec.within("ul");
        } else if tag == "tr" && !in_table {
            spec = spec.within("table");
        }
        spec
    }

    /// `mapType`: the element a structure type is written as.
    fn map_type(&self, node: usize) -> ElementSpec {
        let pdf = self.pdf;
        let index = self.index();
        let structure_type = index.nodes[node].structure_type.as_str();
        let dict = index.nodes[node].dict;
        let mut attrs: Vec<(String, String)> = Vec::new();
        add_attribute(&mut attrs, "lang", pdf.text(dict, b"Lang"));
        let title = pdf.text(dict, b"T");
        add_attribute(&mut attrs, "title", title.or_else(|| pdf.text(dict, b"E")));
        let block = |tag: &str, attrs: Vec<(String, String)>| ElementSpec::new(tag, attrs, false);
        let inline = |tag: &str, attrs: Vec<(String, String)>| ElementSpec::new(tag, attrs, true);
        match structure_type {
            "Document" | "NonStruct" | "LBody" | "Sub" => return ElementSpec::transparent(),
            "Span" => {
                return if attrs.is_empty() {
                    ElementSpec::transparent()
                } else {
                    inline("span", attrs)
                }
            }
            "P" => return block("p", attrs),
            "H" => {
                let level = (1 + index.count_ancestors(node, "Sect")).min(6);
                return block(&format!("h{level}"), attrs);
            }
            "H1" | "H2" | "H3" | "H4" | "H5" | "H6" => {
                return block(&structure_type.to_lowercase(), attrs)
            }
            "Title" => {
                add_attribute(&mut attrs, "class", Some("title".to_owned()));
                return block("h1", attrs);
            }
            "BlockQuote" => return block("blockquote", attrs),
            "Caption" => {
                if self.ancestor_spec(node).and_then(ElementSpec::tag) == Some("table") {
                    return block("caption", attrs);
                }
            }
            "TOC" => {
                add_attribute(&mut attrs, "class", Some("toc".to_owned()));
                return block("ul", attrs);
            }
            "TOCI" | "LI" => return block("li", attrs),
            "L" => {
                let ordered = attributes(pdf, dict).iter().any(|attribute| {
                    pdf.name_as_string(attribute, b"O").as_deref() == Some("List")
                        && pdf.name_as_string(attribute, b"ListNumbering").is_some_and(
                            |numbering| ORDERED_LIST_NUMBERING.contains(&numbering.as_str()),
                        )
                });
                return block(if ordered { "ol" } else { "ul" }, attrs);
            }
            "Table" => {
                for attribute in attributes(pdf, dict) {
                    if pdf.name_as_string(attribute, b"O").as_deref() == Some("Table") {
                        add_attribute(&mut attrs, "summary", pdf.text(attribute, b"Summary"));
                    }
                }
                return block("table", attrs);
            }
            "TR" | "THead" | "TBody" | "TFoot" => {
                return block(&structure_type.to_lowercase(), attrs)
            }
            "TH" | "TD" => {
                self.cell_attributes(dict, &mut attrs, structure_type == "TH");
                return block(&structure_type.to_lowercase(), attrs);
            }
            "Quote" => return inline("q", attrs),
            "Code" => return inline("code", attrs),
            "Em" | "Strong" | "Ruby" | "RB" | "RT" | "RP" => {
                return inline(&structure_type.to_lowercase(), attrs)
            }
            "Link" => {
                return match structure::link_uri(pdf, index.nodes[node].objr) {
                    Some(uri) => {
                        add_attribute(&mut attrs, "href", Some(uri));
                        inline("a", attrs)
                    }
                    None => {
                        add_attribute(&mut attrs, "class", Some("link".to_owned()));
                        inline("span", attrs)
                    }
                }
            }
            "Lbl" | "Reference" | "Annot" | "Warichu" | "WT" | "WP" => {
                add_attribute(&mut attrs, "class", Some(structure_type.to_lowercase()));
                return inline("span", attrs);
            }
            "Figure" | "Formula" => {
                let alt = pdf.text(dict, b"Alt");
                add_attribute(&mut attrs, "alt", alt.clone());
                add_attribute(&mut attrs, "class", Some(structure_type.to_lowercase()));
                let mut spec = ElementSpec::new("div", attrs, false);
                spec.alt = alt
                    .filter(|alt| !is_blank(alt))
                    .map(|alt| crate::java::trim(&alt).to_owned());
                return spec;
            }
            _ => {}
        }
        let class = class_name(structure_type);
        if is_standard_type(structure_type) || !self.inline_context(node) {
            add_attribute(&mut attrs, "class", Some(class));
            return block("div", attrs);
        }
        add_attribute(&mut attrs, "class", Some(class));
        inline("span", attrs)
    }

    /// `cellAttributes`.
    fn cell_attributes(
        &self,
        dict: &lopdf::Dictionary,
        attrs: &mut Vec<(String, String)>,
        header: bool,
    ) {
        let pdf = self.pdf;
        for attribute in attributes(pdf, dict) {
            if pdf.name_as_string(attribute, b"O").as_deref() != Some("Table") {
                continue;
            }
            let row_span = attribute_int(pdf, attribute, b"RowSpan", 1);
            if row_span > 1 {
                add_attribute(attrs, "rowspan", Some(row_span.to_string()));
            }
            let col_span = attribute_int(pdf, attribute, b"ColSpan", 1);
            if col_span > 1 {
                add_attribute(attrs, "colspan", Some(col_span.to_string()));
            }
            if let Some(lopdf::Object::Array(headers)) = pdf.get(attribute, b"Headers") {
                if !headers.is_empty() {
                    let names: Vec<String> = headers
                        .iter()
                        .map(|item| match pdf.resolve(item) {
                            Some(lopdf::Object::String(bytes, _)) => text_string(bytes),
                            _ => "null".to_owned(),
                        })
                        .collect();
                    add_attribute(attrs, "headers", Some(names.join(" ")));
                }
            }
            if header {
                add_attribute(attrs, "scope", pdf.name_as_string(attribute, b"Scope"));
            }
        }
    }

    /// `blockOf`: the nearest ancestor written as a block, or the leaf's own node.
    fn block_of(&self, leaf_node: usize) -> usize {
        let index = self.index();
        let mut current = Some(leaf_node);
        while let Some(node) = current {
            let spec = self.spec(node);
            if spec.tag.is_some() && !spec.inline {
                return node;
            }
            current = index.nodes[node].parent;
        }
        leaf_node
    }

    /// `wrapperTag`: what text straight under a block is wrapped in, if anything.
    fn wrapper_tag(&self, block: usize) -> Option<&'static str> {
        let spec = self.spec(block);
        match spec.tag() {
            None => Some("p"),
            Some(_) if spec.inline => Some("p"),
            Some("div") => Some("p"),
            Some("table" | "thead" | "tbody" | "tfoot") => Some("caption"),
            Some("tr") => Some("td"),
            Some("ul" | "ol") => Some("li"),
            Some(_) => None,
        }
    }

    fn wrapper_for(&self, block: usize) -> Option<Wrapper> {
        let tag = self.wrapper_tag(block)?;
        let spec = self.spec(block);
        Some(Wrapper {
            tag,
            inside: spec.tag.is_some() && !spec.inline,
            at: 0,
            open: false,
            break_pending: false,
        })
    }

    fn in_table_cell(&self, node: usize) -> bool {
        let index = self.index();
        let mut current = Some(node);
        while let Some(n) = current {
            if matches!(index.nodes[n].structure_type.as_str(), "TD" | "TH") {
                return true;
            }
            current = index.nodes[n].parent;
        }
        false
    }

    fn root_of(&self, node: usize) -> usize {
        let index = self.index();
        let mut current = node;
        while let Some(parent) = index.nodes[current].parent {
            current = parent;
        }
        current
    }
}

/// The page's runs, as `runs()` makes them from the recorded calls.
fn runs(events: &[Event]) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for event in events {
        match event {
            Event::Word { segments, .. } => {
                for segment in segments {
                    let new_run = runs.last().is_none_or(|run| run.label != segment.label);
                    if new_run {
                        let ordinal = runs.len();
                        runs.push(Run {
                            label: segment.label,
                            ordinal,
                            pieces: Vec::new(),
                            has_text: false,
                        });
                    }
                    let run = runs.last_mut().unwrap_or_else(|| unreachable!());
                    run.pieces.push(Piece::Text(segment.text.clone()));
                    if !is_blank(&segment.text) {
                        run.has_text = true;
                    }
                }
            }
            Event::WordSeparator => {
                if let Some(run) = runs.last_mut() {
                    run.pieces.push(Piece::Text(WORD_SEPARATOR.to_owned()));
                }
            }
            Event::LineSeparator => {
                if let Some(run) = runs.last_mut() {
                    run.pieces.push(Piece::Newline);
                }
            }
            Event::ParagraphStart | Event::ParagraphEnd => {
                if let Some(run) = runs.last_mut() {
                    run.pieces.push(Piece::ParagraphBreak);
                }
            }
        }
    }
    runs
}

/// `separatedByOthers`.
fn separated_by_others(page_runs: &[Run], previous: &Run, run: &Run) -> bool {
    if run.ordinal == previous.ordinal + 1 || previous.ends_with_separator() {
        return false;
    }
    (previous.ordinal + 1..run.ordinal.min(page_runs.len()))
        .any(|index| page_runs[index].has_whitespace())
}

impl<'p> Tagged<'p> {
    /// `emitTagged`.
    fn emit_tagged(writer: &mut PageWriter<'p, '_, '_>, content: &PageContent) -> Result<(), Stop> {
        let tagged = Self::of(writer);
        let page_runs = runs(&tagged.events);
        let mut group_of: HashMap<usize, usize> = HashMap::new();
        let mut groups: Vec<BlockGroup> = Vec::new();
        let mut untagged: Vec<usize> = Vec::new();
        let mut artifacts: Vec<usize> = Vec::new();
        for (index, run) in page_runs.iter().enumerate() {
            if run.label == Label::Artifact {
                artifacts.push(index);
                continue;
            }
            let Some(leaf) = tagged.leaf(run.label) else {
                untagged.push(index);
                continue;
            };
            let group = tagged.group(&mut group_of, &mut groups, leaf);
            groups[group].add(Some(index), leaf, run.has_text);
        }
        let structure = tagged.index();
        let object_leaves: Vec<Leaf> = content
            .scopes
            .iter()
            .flat_map(|scope| structure.object_leaves(*scope).to_vec())
            .collect();
        for leaf in object_leaves {
            let group = tagged.group(&mut group_of, &mut groups, leaf);
            groups[group].add(None, leaf, false);
        }
        groups.sort_by_key(|group| group.first_leaf_order);
        let mut merged: Vec<BlockGroup> = Vec::with_capacity(groups.len());
        for group in groups {
            let absorb = merged.last().is_some_and(|last: &BlockGroup| {
                last.loose
                    && group.loose
                    && tagged.root_of(last.block) == tagged.root_of(group.block)
            });
            if absorb {
                let last = merged.last_mut().unwrap_or_else(|| unreachable!());
                last.absorb(group, &page_runs);
            } else {
                merged.push(group);
            }
        }
        tagged.open.clear();
        tagged.open_state.clear();
        tagged.alt_written.clear();
        tagged.held.clear();
        tagged.at_block_start = false;
        for group in &merged {
            let tagged = Self::of(writer);
            if !group.has_text && !group.has_object && !tagged.in_table_cell(group.block) {
                continue;
            }
            let mut wrapper = if group.has_text {
                tagged.wrapper_for(group.block)
            } else {
                None
            };
            let mut previous: Option<usize> = None;
            for (position, run) in group.runs.iter().enumerate() {
                let tagged = Self::of(writer);
                let mut path: Vec<usize> = Vec::new();
                let mut current = Some(group.leaves[position].node);
                while let Some(node) = current {
                    path.push(node);
                    current = tagged.index().nodes[node].parent;
                }
                path.reverse();
                let mut common = 0;
                while common < tagged.open.len()
                    && common < path.len()
                    && tagged.open[common] == path[common]
                {
                    common += 1;
                }
                let writable = tagged.writable(&path);
                if let Some(reopened) = (0..common)
                    .find(|&i| tagged.open_state[i] == ElementState::NotWritten && writable[i])
                {
                    common = reopened;
                }
                Self::close_open_from(writer, common, wrapper.as_mut())?;
                let tagged = Self::of(writer);
                if let Some(wrapper) = wrapper.as_mut() {
                    wrapper.at = tagged.wrapper_start(&path, group.block, wrapper.inside);
                }
                let write = tagged.within_budget(&path, common, &writable);
                for i in common..path.len() {
                    if let Some(wrapper) = wrapper.as_mut() {
                        if !wrapper.open && i >= wrapper.at && write[i - common] {
                            Self::open_wrapper(writer, wrapper)?;
                        }
                    }
                    let wrote = write[i - common] && Self::open_element(writer, path[i])?;
                    let tagged = Self::of(writer);
                    tagged.open.push(path[i]);
                    tagged.open_state.push(if wrote {
                        ElementState::Open
                    } else if writable[i] {
                        ElementState::Dropped
                    } else {
                        ElementState::NotWritten
                    });
                }
                let Some(run) = run else {
                    continue;
                };
                if let Some(previous) = previous {
                    let separated =
                        separated_by_others(&page_runs, &page_runs[previous], &page_runs[*run]);
                    if separated && wrapper.as_ref().is_none_or(|w| w.open) {
                        Self::sink_text(writer, " ")?;
                    }
                }
                previous = Some(*run);
                Self::write_pieces(writer, &page_runs[*run].pieces, wrapper.as_mut())?;
            }
            if let Some(wrapper) = wrapper.as_mut() {
                let at = wrapper.at;
                Self::close_open_from(writer, at, Some(wrapper))?;
                if wrapper.open {
                    Self::close_wrapper(writer, wrapper)?;
                }
            }
        }
        Self::close_open_from(writer, 0, None)?;
        Self::of(writer).held.clear();
        Self::emit_blocks(writer, "untagged", &page_runs, &untagged)?;
        Self::emit_blocks(writer, "artifact", &page_runs, &artifacts)?;
        Ok(())
    }

    fn group(
        &self,
        group_of: &mut HashMap<usize, usize>,
        groups: &mut Vec<BlockGroup>,
        leaf: Leaf,
    ) -> usize {
        let block = self.block_of(leaf.node);
        if let Some(&group) = group_of.get(&block) {
            return group;
        }
        let spec = self.spec(block);
        let loose = spec.tag.is_none() || spec.inline;
        groups.push(BlockGroup {
            block,
            loose,
            runs: Vec::new(),
            leaves: Vec::new(),
            first_leaf_order: usize::MAX,
            has_text: false,
            has_object: false,
        });
        group_of.insert(block, groups.len() - 1);
        groups.len() - 1
    }

    /// `writable`: which nodes of the path write an element, before the nesting budget.
    fn writable(&self, path: &[usize]) -> Vec<bool> {
        let mut writable = vec![false; path.len()];
        let mut block_below = false;
        let mut demoted_below = false;
        for i in (0..path.len()).rev() {
            let spec = self.spec(path[i]);
            writable[i] = spec.tag.is_some();
            let Some(tag) = spec.tag() else {
                continue;
            };
            if spec.demoted {
                if block_below || demoted_below {
                    writable[i] = false;
                }
                demoted_below = true;
            } else if !spec.inline {
                if block_below && is_text_block(tag) {
                    writable[i] = false;
                }
                block_below = true;
            }
        }
        writable
    }

    /// `withinBudget`: which nodes about to open may write an element under the nesting limit.
    fn within_budget(&self, path: &[usize], from: usize, writable: &[bool]) -> Vec<bool> {
        let mut write = writable[from..].to_vec();
        let mut count = 0usize;
        let mut text_block_open = false;
        let mut demoted_open = false;
        for i in 0..from {
            if self.open_state[i] == ElementState::Open {
                let spec = self.spec(self.open[i]);
                count += spec.elements();
                if spec.demoted {
                    demoted_open = true;
                } else if !spec.inline {
                    text_block_open = spec.tag().is_some_and(is_text_block);
                }
            }
        }
        for i in from..path.len() {
            if !write[i - from] {
                continue;
            }
            let spec = self.spec(path[i]);
            count += spec.elements();
            if (spec.demoted || !spec.inline) && demoted_open {
                count -= 1;
                demoted_open = false;
            }
            if !spec.inline {
                if text_block_open {
                    count -= 1;
                }
                text_block_open = spec.tag().is_some_and(is_text_block);
            }
        }
        for pass in 0..2 {
            if count <= MAX_OPEN_ELEMENTS {
                break;
            }
            for i in from..path.len() {
                if count <= MAX_OPEN_ELEMENTS {
                    break;
                }
                let structure_type = &self.index().nodes[path[i]].structure_type;
                if write[i - from]
                    && (pass == 1
                        || GROUPING_TYPES.contains(&structure_type.as_str())
                        || !is_standard_type(structure_type))
                {
                    write[i - from] = false;
                    count -= self.spec(path[i]).elements();
                }
            }
        }
        write
    }

    /// `wrapperStart`.
    fn wrapper_start(&self, path: &[usize], block: usize, inside: bool) -> usize {
        if inside {
            return path
                .iter()
                .position(|&node| node == block)
                .map_or(0, |index| index + 1);
        }
        path.iter()
            .position(|&node| self.spec(node).tag.is_some())
            .unwrap_or(path.len() - 1)
    }

    /// `closeOpenFrom`.
    fn close_open_from(
        writer: &mut PageWriter<'p, '_, '_>,
        from: usize,
        mut wrapper: Option<&mut Wrapper>,
    ) -> Result<(), Stop> {
        let length = Self::of(writer).open.len();
        for i in (from..length).rev() {
            if let Some(wrapper) = wrapper.as_deref_mut() {
                if wrapper.open && wrapper.inside && wrapper.at >= 1 && i == wrapper.at - 1 {
                    Self::close_wrapper(writer, wrapper)?;
                }
            }
            let tagged = Self::of(writer);
            let state = tagged.open_state.remove(i);
            let node = tagged.open[i];
            match state {
                ElementState::Open => Self::close(writer, node)?,
                ElementState::Suspended => Self::close_outer(writer, node)?,
                ElementState::NotWritten | ElementState::Dropped => {}
            }
            Self::of(writer).open.remove(i);
        }
        if let Some(wrapper) = wrapper {
            if wrapper.open && wrapper.break_pending && !Self::of(writer).written_inside(wrapper.at)
            {
                Self::close_wrapper(writer, wrapper)?;
            }
        }
        Ok(())
    }

    fn written_inside(&self, at: usize) -> bool {
        self.open_state
            .iter()
            .skip(at)
            .any(|state| *state == ElementState::Open)
    }

    /// `suspendTextBlock`.
    fn suspend_text_block(writer: &mut PageWriter<'p, '_, '_>) -> Result<(), Stop> {
        let tagged = Self::of(writer);
        for i in (0..tagged.open.len()).rev() {
            if tagged.open_state[i] != ElementState::Open {
                continue;
            }
            let spec = tagged.spec(tagged.open[i]).clone();
            if spec.inline {
                continue;
            }
            if let Some(tag) = spec.tag().filter(|tag| is_text_block(tag)) {
                Self::sink_close(writer, tag, false)?;
                Self::of(writer).open_state[i] = ElementState::Suspended;
            }
            return Ok(());
        }
        Ok(())
    }

    /// `suspendDemoted`.
    fn suspend_demoted(writer: &mut PageWriter<'p, '_, '_>) -> Result<(), Stop> {
        let tagged = Self::of(writer);
        for i in (0..tagged.open.len()).rev() {
            if tagged.open_state[i] != ElementState::Open {
                continue;
            }
            let spec = tagged.spec(tagged.open[i]).clone();
            if spec.demoted {
                Self::sink_close(writer, spec.tag().unwrap_or_default(), true)?;
                Self::sink_boundary(writer);
                Self::of(writer).open_state[i] = ElementState::Suspended;
                return Ok(());
            }
            if !spec.inline {
                return Ok(());
            }
        }
        Ok(())
    }

    fn open_wrapper(
        writer: &mut PageWriter<'p, '_, '_>,
        wrapper: &mut Wrapper,
    ) -> Result<(), Stop> {
        Self::sink_open(writer, wrapper.tag, &[], false)?;
        wrapper.open = true;
        wrapper.break_pending = false;
        Ok(())
    }

    fn close_wrapper(
        writer: &mut PageWriter<'p, '_, '_>,
        wrapper: &mut Wrapper,
    ) -> Result<(), Stop> {
        Self::sink_close(writer, wrapper.tag, false)?;
        wrapper.open = false;
        wrapper.break_pending = false;
        Ok(())
    }

    /// `sinkText`: text with whitespace at a block's start dropped and trailing whitespace held.
    fn sink_text(writer: &mut PageWriter<'p, '_, '_>, text: &str) -> Result<(), Stop> {
        let tagged = Self::of(writer);
        let mut text = text;
        if tagged.at_block_start {
            let trimmed = text.trim_start_matches(crate::unicode::is_whitespace);
            if trimmed.is_empty() {
                return Ok(());
            }
            text = trimmed;
            tagged.at_block_start = false;
        }
        let kept = text.trim_end_matches(crate::unicode::is_whitespace);
        if kept.is_empty() {
            tagged.held.push(Piece::Text(text.to_owned()));
            return Ok(());
        }
        let trailing = text[kept.len()..].to_owned();
        Self::flush_held(writer)?;
        writer.characters(kept)?;
        if !trailing.is_empty() {
            Self::of(writer).held.push(Piece::Text(trailing));
        }
        Ok(())
    }

    fn sink_newline(writer: &mut PageWriter<'p, '_, '_>) {
        let tagged = Self::of(writer);
        if !tagged.at_block_start {
            tagged.held.push(Piece::Newline);
        }
    }

    fn sink_boundary(writer: &mut PageWriter<'p, '_, '_>) {
        let last_newline = Self::of(writer).held.last() == Some(&Piece::Newline);
        if !last_newline {
            Self::sink_newline(writer);
        }
    }

    fn flush_held(writer: &mut PageWriter<'p, '_, '_>) -> Result<(), Stop> {
        let held = std::mem::take(&mut Self::of(writer).held);
        for piece in held {
            match piece {
                Piece::Newline => writer.newline()?,
                Piece::Text(text) => writer.characters(&text)?,
                Piece::ParagraphBreak => {}
            }
        }
        Ok(())
    }

    fn sink_open(
        writer: &mut PageWriter<'p, '_, '_>,
        tag: &str,
        attributes: &[(String, String)],
        inline: bool,
    ) -> Result<(), Stop> {
        Self::flush_held(writer)?;
        let borrowed: Vec<(&str, &str)> = attributes
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        writer.start(tag, &borrowed)?;
        if !inline {
            Self::of(writer).at_block_start = true;
        }
        Ok(())
    }

    fn sink_close(
        writer: &mut PageWriter<'p, '_, '_>,
        tag: &str,
        inline: bool,
    ) -> Result<(), Stop> {
        if !inline {
            let tagged = Self::of(writer);
            tagged.held.clear();
            tagged.at_block_start = false;
        }
        writer.end(tag)?;
        if tag == "caption" {
            writer.newline()?;
        }
        Ok(())
    }

    /// `openElement`: the node's element written, if it has one.
    fn open_element(writer: &mut PageWriter<'p, '_, '_>, node: usize) -> Result<bool, Stop> {
        let spec = Self::of(writer).spec(node).clone();
        let Some(tag) = spec.tag() else {
            return Ok(false);
        };
        if spec.demoted || !spec.inline {
            Self::suspend_demoted(writer)?;
        }
        if !spec.inline {
            Self::suspend_text_block(writer)?;
        }
        if let Some(outer) = spec.outer {
            Self::sink_open(writer, outer, &[], false)?;
        }
        if spec.demoted {
            Self::sink_boundary(writer);
        }
        Self::sink_open(writer, tag, &spec.attributes, spec.inline)?;
        if let Some(alt) = &spec.alt {
            if Self::of(writer).alt_written.insert(node) {
                Self::sink_text(writer, alt)?;
                Self::sink_newline(writer);
            }
        }
        Ok(true)
    }

    fn close(writer: &mut PageWriter<'p, '_, '_>, node: usize) -> Result<(), Stop> {
        let spec = Self::of(writer).spec(node).clone();
        if let Some(tag) = spec.tag() {
            Self::sink_close(writer, tag, spec.inline)?;
            if spec.demoted {
                Self::sink_boundary(writer);
            }
            Self::close_outer(writer, node)?;
        }
        Ok(())
    }

    fn close_outer(writer: &mut PageWriter<'p, '_, '_>, node: usize) -> Result<(), Stop> {
        let outer = Self::of(writer).spec(node).outer;
        if let Some(outer) = outer {
            Self::sink_close(writer, outer, false)?;
        }
        Ok(())
    }

    /// `writePieces`.
    fn write_pieces(
        writer: &mut PageWriter<'p, '_, '_>,
        pieces: &[Piece],
        mut wrapper: Option<&mut Wrapper>,
    ) -> Result<(), Stop> {
        for piece in pieces {
            match piece {
                Piece::ParagraphBreak => {
                    if let Some(wrapper) = wrapper.as_deref_mut() {
                        if wrapper.open {
                            if wrapper.tag != "p" {
                                Self::sink_newline(writer);
                            } else if Self::of(writer).written_inside(wrapper.at) {
                                wrapper.break_pending = true;
                            } else {
                                Self::close_wrapper(writer, wrapper)?;
                            }
                        }
                    }
                }
                Piece::Newline => {
                    if wrapper.as_ref().is_none_or(|w| w.open) {
                        Self::sink_newline(writer);
                    }
                }
                Piece::Text(text) => {
                    if let Some(wrapper) = wrapper.as_deref_mut() {
                        if !wrapper.open {
                            if is_blank(text) {
                                continue;
                            }
                            Self::open_wrapper(writer, wrapper)?;
                        }
                    }
                    Self::sink_text(writer, text)?;
                }
            }
        }
        Ok(())
    }

    /// `emitBlocks`: text the tree does not own, in paragraphs.
    fn emit_blocks(
        writer: &mut PageWriter<'p, '_, '_>,
        class: &str,
        page_runs: &[Run],
        runs: &[usize],
    ) -> Result<(), Stop> {
        if !runs.iter().any(|&run| page_runs[run].has_text) {
            return Ok(());
        }
        Self::sink_open(
            writer,
            "div",
            &[("class".to_owned(), class.to_owned())],
            false,
        )?;
        let mut in_paragraph = false;
        let mut previous: Option<usize> = None;
        for &index in runs {
            let run = &page_runs[index];
            if !run.has_text && !in_paragraph {
                continue;
            }
            if let (true, Some(previous)) = (in_paragraph, previous) {
                if separated_by_others(page_runs, &page_runs[previous], run) {
                    Self::sink_text(writer, " ")?;
                }
            }
            previous = Some(index);
            for piece in &run.pieces {
                if *piece == Piece::ParagraphBreak {
                    if in_paragraph {
                        Self::sink_close(writer, "p", false)?;
                        in_paragraph = false;
                    }
                    continue;
                }
                if !in_paragraph {
                    Self::sink_open(writer, "p", &[], false)?;
                    in_paragraph = true;
                }
                match piece {
                    Piece::Newline => Self::sink_newline(writer),
                    Piece::Text(text) => Self::sink_text(writer, text)?,
                    Piece::ParagraphBreak => {}
                }
            }
        }
        if in_paragraph {
            Self::sink_close(writer, "p", false)?;
        }
        Self::sink_close(writer, "div", false)
    }
}

fn is_text_block(tag: &str) -> bool {
    tag == "p" || (tag.len() == 2 && tag.starts_with('h'))
}

impl BlockGroup {
    fn add(&mut self, run: Option<usize>, leaf: Leaf, has_text: bool) {
        self.runs.push(run);
        self.leaves.push(leaf);
        self.first_leaf_order = self.first_leaf_order.min(leaf.order);
        match run {
            None => self.has_object = true,
            Some(_) => self.has_text |= has_text,
        }
    }

    /// `absorb`: another group's runs taken in, in content order.
    fn absorb(&mut self, other: BlockGroup, page_runs: &[Run]) {
        let mut i = 0;
        for (j, run) in other.runs.iter().enumerate() {
            match run {
                None => i = self.runs.len(),
                Some(run) => {
                    while i < self.runs.len()
                        && self.runs[i].is_some_and(|existing| {
                            page_runs[existing].ordinal < page_runs[*run].ordinal
                        })
                    {
                        i += 1;
                    }
                }
            }
            self.runs.insert(i, *run);
            self.leaves.insert(i, other.leaves[j]);
            i += 1;
        }
        self.has_text |= other.has_text;
        self.has_object |= other.has_object;
    }
}
