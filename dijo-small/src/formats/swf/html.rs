//! The HTML text of a text field, in the subset of HTML Flash renders, built as HTML is
//! ([`walk_parsed`]) and written as paragraphs: each `<p>` a paragraph and each `<li>` an item
//! of a list, `<br>` a line break and text outside them a paragraph of its own; a block without
//! text is left out. The text of
//! `head`, `script`, `style` and `title` is left out, and the other elements, Flash's `font`,
//! `b`, `i`, `u`, `a`, `span`, `img` and `textformat`, are written as their text.

use crate::formats::html::{walk_parsed, ParsedNode, Visitor};
use crate::formats::xhtml::{Stop, Xhtml};

/// Writes the text field's HTML `html`.
pub(super) fn write(html: &str, xhtml: &mut Xhtml) -> Result<(), Stop> {
    let mut writer = Writer {
        xhtml,
        block: Block::None,
        list: false,
        open: Vec::new(),
        left_out: 0,
    };
    walk_parsed(html, &mut writer)?;
    writer.close_block()?;
    writer.close_list()
}

/// What a block is written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Paragraph,
    Item,
}

/// Where the writer is in its blocks: a block is written once it has content, so that an empty
/// one is left out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    None,
    Pending(Kind),
    Open(Kind),
}

/// What ending an element ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Open {
    Nothing,
    Block,
}

struct Writer<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    block: Block,
    /// Whether a list is open, for the items in it.
    list: bool,
    open: Vec<Open>,
    /// How deep the walk is in an element whose text is left out, none outside one.
    left_out: usize,
}

impl Writer<'_, '_> {
    fn close_block(&mut self) -> Result<(), Stop> {
        match std::mem::replace(&mut self.block, Block::None) {
            Block::None | Block::Pending(_) => Ok(()),
            Block::Open(Kind::Paragraph) => self.xhtml.end_element("p"),
            Block::Open(Kind::Item) => self.xhtml.end_element("li"),
        }
    }

    fn close_list(&mut self) -> Result<(), Stop> {
        if std::mem::take(&mut self.list) {
            self.xhtml.end_element("ul")?;
        }
        Ok(())
    }

    fn start_block(&mut self, kind: Kind) -> Result<(), Stop> {
        self.close_block()?;
        self.block = Block::Pending(kind);
        Ok(())
    }

    /// Writes the start of the block content goes in, a paragraph outside any.
    fn in_block(&mut self) -> Result<(), Stop> {
        let kind = match self.block {
            Block::Open(_) => return Ok(()),
            Block::Pending(kind) => kind,
            Block::None => Kind::Paragraph,
        };
        match kind {
            Kind::Paragraph => {
                self.close_list()?;
                self.xhtml.start_element("p")?;
            }
            Kind::Item => {
                if !self.list {
                    self.xhtml.start_element("ul")?;
                    self.list = true;
                }
                self.xhtml.start_element("li")?;
            }
        }
        self.block = Block::Open(kind);
        Ok(())
    }
}

impl Visitor for Writer<'_, '_> {
    type Stop = Stop;

    fn start(&mut self, node: &ParsedNode<'_>) -> Result<(), Stop> {
        if self.left_out > 0 {
            self.left_out += 1;
            return Ok(());
        }
        let open = match node {
            ParsedNode::Element { name, .. } => match *name {
                "head" | "script" | "style" | "title" => {
                    self.left_out = 1;
                    return Ok(());
                }
                "p" => {
                    self.start_block(Kind::Paragraph)?;
                    Open::Block
                }
                "li" => {
                    self.start_block(Kind::Item)?;
                    Open::Block
                }
                "br" => {
                    self.in_block()?;
                    self.xhtml.start_element("br")?;
                    self.xhtml.end_element("br")?;
                    Open::Nothing
                }
                _ => Open::Nothing,
            },
            ParsedNode::Document | ParsedNode::Comment | ParsedNode::Doctype => Open::Nothing,
        };
        self.open.push(open);
        Ok(())
    }

    fn text(&mut self, text: &str) -> Result<(), Stop> {
        if self.left_out > 0 {
            return Ok(());
        }
        if !matches!(self.block, Block::Open(_)) && text.trim().is_empty() {
            return Ok(());
        }
        self.in_block()?;
        self.xhtml.characters(text)
    }

    fn end(&mut self, _node: &ParsedNode<'_>) -> Result<(), Stop> {
        if self.left_out > 0 {
            self.left_out -= 1;
            return Ok(());
        }
        match self.open.pop() {
            Some(Open::Block) => self.close_block(),
            Some(Open::Nothing) | None => Ok(()),
        }
    }
}
