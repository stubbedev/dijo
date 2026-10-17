//! Tika's `ImageGraphicsEngine`, a `PDFGraphicsStreamEngine` that finds the images a page draws:
//! its content stream and the forms, transparency groups and tiling patterns it paints with,
//! then the transparency groups of its graphics states' soft masks. The stream engine's work
//! that does not bear on which images are drawn, such as the transformation matrices and the
//! clipping, is left out.

use lopdf::{Dictionary, Object, Stream};

use super::super::content::{InlineImage, Lexer, Token};
use super::super::cos::{as_dict, int_value, Identity, Pdf};
use super::super::document::Page;
use super::super::writer::WriteFailure;
use super::super::Unported;

/// `DrawObject`'s limit on forms drawing forms.
const MAX_FORM_LEVEL: i32 = 50;
/// How deep the port follows content streams painting with content streams, where PDFBox
/// follows them until its stack overflows.
const MAX_NESTING: usize = 100;

/// An image a page draws, with the resources current where it is drawn.
pub(super) enum Drawn<'p> {
    XObject {
        identity: Identity,
        stream: &'p Stream,
        resources: Option<&'p Dictionary>,
    },
    Inline {
        image: InlineImage,
        resources: Option<&'p Dictionary>,
    },
}

/// What `found` is handed each image the page draws with.
pub(super) type Found<'f, 'p> = &'f mut dyn FnMut(Drawn<'p>) -> Result<(), WriteFailure>;

/// What a colour paints with, as far as the images it draws go: a colour, or a pattern of
/// the `Pattern` colour space, named by the colour, in the resources current when the colour
/// space was set.
#[derive(Debug, Clone)]
enum Paint<'p> {
    Plain,
    Pattern {
        resources: Option<&'p Dictionary>,
        name: Option<Vec<u8>>,
    },
}

/// The graphics state, as far as the images drawn go.
#[derive(Debug, Clone)]
struct State<'p> {
    fill: Paint<'p>,
    stroke: Paint<'p>,
    /// `RenderingMode`: whether text is filled, stroked, both or neither.
    rendering_mode: i32,
}

impl State<'_> {
    fn new() -> Self {
        Self {
            fill: Paint::Plain,
            stroke: Paint::Plain,
            rendering_mode: 0,
        }
    }
}

/// What kind of content stream is processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Content {
    /// A page, a form or a transparency group.
    Stream,
    /// A tiling pattern, which paints with colours of its own or, uncoloured, with none.
    Pattern { coloured: bool },
}

struct Engine<'p, 'f> {
    pdf: &'p Pdf,
    page: Page<'p>,
    resources: Option<&'p Dictionary>,
    stack: Vec<State<'p>>,
    level: i32,
    nesting: usize,
    /// `shouldProcessColorOperators`.
    color_operators: bool,
    /// Whether a text object is open, outside which PDFBox shows no text.
    in_text: bool,
    /// Whether images are only looked for, for text painted with a pattern.
    probing: bool,
    probed: bool,
    found: Found<'f, 'p>,
}

/// `ImageGraphicsEngine.run`: the images `page` draws, handed to `found` in the order PDFBox
/// draws them.
pub(super) fn run<'p>(
    pdf: &'p Pdf,
    page: Page<'p>,
    found: Found<'_, 'p>,
) -> Result<(), WriteFailure> {
    let mut engine = Engine {
        pdf,
        page,
        resources: None,
        stack: vec![State::new()],
        level: 0,
        nesting: 0,
        color_operators: true,
        in_text: false,
        probing: false,
        probed: false,
        found,
    };
    if page.has_contents(pdf) {
        let data = page.contents(pdf)?;
        engine.process_stream(&data, page.resources(pdf), Content::Stream)?;
    }
    let Some(resources) = page.resources(pdf) else {
        return Ok(());
    };
    let Some(states) = pdf.dict(resources, b"ExtGState") else {
        return Ok(());
    };
    for (name, _) in states.iter() {
        let Some(state) = pdf.dict(states, name) else {
            continue;
        };
        let Some(group) = pdf
            .dict(state, b"SMask")
            .and_then(|mask| pdf.get(mask, b"G"))
            .and_then(|group| transparency_group(pdf, group))
        else {
            continue;
        };
        engine.stack.push(State::new());
        let processed = engine.process_group(group);
        engine.stack.pop();
        processed?;
    }
    Ok(())
}

/// The stream of a form whose group is a transparency group, as `PDXObject.createXObject`
/// makes a `PDTransparencyGroup` of it.
fn transparency_group<'p>(pdf: &'p Pdf, object: &'p Object) -> Option<&'p Stream> {
    let Object::Stream(stream) = object else {
        return None;
    };
    (is_form(pdf, stream) && is_transparency_group(pdf, stream)).then_some(stream)
}

fn is_form(pdf: &Pdf, stream: &Stream) -> bool {
    pdf.name(&stream.dict, b"Subtype") == Some(b"Form")
}

fn is_transparency_group(pdf: &Pdf, stream: &Stream) -> bool {
    pdf.dict(&stream.dict, b"Group")
        .and_then(|group| pdf.name(group, b"S"))
        == Some(b"Transparency")
}

impl<'p> Engine<'p, '_> {
    fn state(&mut self) -> &mut State<'p> {
        self.stack
            .last_mut()
            .expect("the graphics stack is never empty")
    }

    /// `processStream` and its kin: a content stream with its own resources, or those current,
    /// or the page's, on a graphics stack of its own that starts with `state`, or a copy of the
    /// current state.
    fn process_with(
        &mut self,
        data: &[u8],
        resources: Option<&'p Dictionary>,
        content: Content,
        state: Option<State<'p>>,
    ) -> Result<(), WriteFailure> {
        self.nesting += 1;
        if self.nesting > MAX_NESTING {
            return Err(Unported(format!(
                "content streams painting with content streams deeper than {MAX_NESTING}, \
                 which PDFBox follows until its stack overflows"
            ))
            .into());
        }
        let parent = self.resources;
        self.resources = resources
            .or(parent)
            .or_else(|| self.page.resources(self.pdf));
        let top = state.unwrap_or_else(|| self.state().clone());
        let saved = std::mem::replace(&mut self.stack, vec![top]);
        let color_operators = self.color_operators;
        self.color_operators = content != Content::Pattern { coloured: false };
        let result = self.process_operators(data);
        self.color_operators = color_operators;
        self.stack = saved;
        self.resources = parent;
        self.nesting -= 1;
        result
    }

    fn process_stream(
        &mut self,
        data: &[u8],
        resources: Option<&'p Dictionary>,
        content: Content,
    ) -> Result<(), WriteFailure> {
        self.process_with(data, resources, content, None)
    }

    /// `processTransparencyGroup`.
    fn process_group(&mut self, group: &'p Stream) -> Result<(), WriteFailure> {
        let data = self.pdf.decode(group)?;
        let resources = self.pdf.dict(&group.dict, b"Resources");
        self.process_stream(&data, resources, Content::Stream)
    }

    fn process_operators(&mut self, data: &[u8]) -> Result<(), WriteFailure> {
        let mut lexer = Lexer::new(data);
        let mut operands: Vec<Object> = Vec::new();
        while let Some(token) = lexer.next_token()? {
            match token {
                Token::Operand(operand) => operands.push(operand),
                Token::Operator(operator) => {
                    self.operator(&operator, &operands)?;
                    operands.clear();
                }
                Token::InlineImage(image) => {
                    self.inline_image(*image)?;
                    operands.clear();
                }
            }
        }
        Ok(())
    }

    fn operator(&mut self, name: &str, operands: &[Object]) -> Result<(), WriteFailure> {
        match name {
            "q" => {
                let top = self.state().clone();
                self.stack.push(top);
            }
            "Q" => {
                if self.stack.len() > 1 {
                    self.stack.pop();
                }
            }
            "cs" | "CS" => self.set_color_space(name == "cs", operands),
            "sc" | "scn" | "SC" | "SCN" => self.set_color(name.starts_with('s'), operands),
            "g" | "rg" | "k" | "G" | "RG" | "K" => {
                if self.color_operators {
                    let paint = if name.chars().all(|c| c.is_ascii_lowercase()) {
                        &mut self.state().fill
                    } else {
                        &mut self.state().stroke
                    };
                    *paint = Paint::Plain;
                }
            }
            "Tr" => {
                if let Some(mode) = operands.first().and_then(int_value) {
                    if !(0..=7).contains(&mode) {
                        return Err(Unported(format!(
                            "the text rendering mode {mode}, which PDFBox fails on"
                        ))
                        .into());
                    }
                    self.state().rendering_mode = mode;
                }
            }
            "BT" => self.in_text = true,
            "ET" => self.in_text = false,
            "Tj" | "'" | "\"" | "TJ" => {
                if self.in_text && shows_glyphs(name, operands) {
                    self.show_text()?;
                }
            }
            "S" | "s" => {
                let paint = self.state().stroke.clone();
                self.process_color(&paint)?;
            }
            "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" => {
                let paint = self.state().fill.clone();
                self.process_color(&paint)?;
            }
            "Do" => self.draw_object(operands)?,
            _ => {}
        }
        Ok(())
    }

    /// `SetNonStrokingColorSpace` and `SetStrokingColorSpace`, as far as patterns go.
    fn set_color_space(&mut self, fill: bool, operands: &[Object]) {
        let Some(Object::Name(name)) = operands.first() else {
            return;
        };
        if !self.color_operators {
            return;
        }
        let pdf = self.pdf;
        let resources = self.resources;
        let named = resources
            .and_then(|resources| pdf.dict(resources, b"ColorSpace"))
            .and_then(|spaces| pdf.get(spaces, name));
        let pattern = match named {
            Some(Object::Name(kind)) => kind == b"Pattern",
            Some(Object::Array(items)) => {
                matches!(items.first().and_then(|first| pdf.resolve(first)), Some(Object::Name(kind)) if kind == b"Pattern")
            }
            Some(_) => false,
            None => match name.as_slice() {
                b"Pattern" => true,
                b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" => false,
                _ => return,
            },
        };
        let paint = if pattern {
            Paint::Pattern {
                resources,
                name: None,
            }
        } else {
            Paint::Plain
        };
        let state = self.state();
        if fill {
            state.fill = paint;
        } else {
            state.stroke = paint;
        }
    }

    /// `SetColor`, as far as patterns go: the pattern a colour of the `Pattern` colour space
    /// names with its last operand.
    fn set_color(&mut self, fill: bool, operands: &[Object]) {
        let state = self.state();
        let paint = if fill {
            &mut state.fill
        } else {
            &mut state.stroke
        };
        if let Paint::Pattern { name, .. } = paint {
            *name = match operands.last() {
                Some(Object::Name(pattern)) => Some(pattern.clone()),
                _ => None,
            };
        }
    }

    /// `ImageGraphicsEngine.processColor`: the images of the tiling pattern a colour paints
    /// with.
    fn process_color(&mut self, paint: &Paint<'p>) -> Result<(), WriteFailure> {
        let Paint::Pattern { resources, name } = paint else {
            return Ok(());
        };
        let pdf = self.pdf;
        let pattern = name.as_ref().and_then(|name| {
            resources
                .and_then(|resources| pdf.dict(resources, b"Pattern"))
                .and_then(|patterns| pdf.get(patterns, name))
        });
        let Some(pattern) = pattern.filter(|pattern| as_dict(pattern).is_some()) else {
            return Err(Unported(
                "a pattern colour whose pattern is missing, which PDFBox fails on".to_owned(),
            )
            .into());
        };
        let dict = as_dict(pattern).expect("filtered to dictionaries");
        match pdf.int(dict, b"PatternType", 0) {
            1 => {}
            2 => return Ok(()),
            _ => {
                return Err(Unported(
                    "a pattern of an unknown type, which PDFBox fails on".to_owned(),
                )
                .into())
            }
        }
        let Object::Stream(stream) = pattern else {
            return Err(Unported(
                "a tiling pattern without content, which PDFBox fails on".to_owned(),
            )
            .into());
        };
        if pdf.array(&stream.dict, b"BBox").is_none() {
            return Err(Unported(
                "a tiling pattern without a bounding box, which PDFBox fails on".to_owned(),
            )
            .into());
        }
        let coloured = pdf.int(&stream.dict, b"PaintType", 0) != 2;
        let data = pdf.decode(stream)?;
        let resources = pdf.dict(&stream.dict, b"Resources");
        self.process_with(
            &data,
            resources,
            Content::Pattern { coloured },
            Some(State::new()),
        )
    }

    /// `ImageGraphicsEngine.showGlyph` for each glyph of a text: the patterns its colours
    /// paint with, which the port follows only when they draw no image, since it does not
    /// count the glyphs.
    fn show_text(&mut self) -> Result<(), WriteFailure> {
        let mode = self.state().rendering_mode;
        let fills = matches!(mode, 0 | 2 | 4 | 6);
        let strokes = matches!(mode, 1 | 2 | 5 | 6);
        let mut paints = Vec::new();
        if fills {
            paints.push(self.state().fill.clone());
        }
        if strokes {
            paints.push(self.state().stroke.clone());
        }
        for paint in paints {
            if !matches!(paint, Paint::Pattern { .. }) {
                continue;
            }
            let probing = std::mem::replace(&mut self.probing, true);
            let probed = std::mem::replace(&mut self.probed, false);
            let result = self.process_color(&paint);
            let draws = self.probed;
            self.probing = probing;
            self.probed = probed || draws;
            result?;
            if draws {
                return Err(Unported(
                    "text painted with a tiling pattern that draws images, which PDFBox draws \
                     once a glyph"
                        .to_owned(),
                )
                .into());
            }
        }
        Ok(())
    }

    /// Hands an image on, or notes that one is drawn while probing.
    fn found(&mut self, drawn: Drawn<'p>) -> Result<(), WriteFailure> {
        if self.probing {
            self.probed = true;
            return Ok(());
        }
        (self.found)(drawn)
    }

    /// `DrawObject`.
    fn draw_object(&mut self, operands: &[Object]) -> Result<(), WriteFailure> {
        let Some(Object::Name(name)) = operands.first() else {
            return Ok(());
        };
        let pdf = self.pdf;
        let Some((identity, object)) = self
            .resources
            .and_then(|resources| pdf.dict(resources, b"XObject"))
            .and_then(|objects| pdf.get_identified(objects, name))
        else {
            return Ok(());
        };
        let Object::Stream(stream) = object else {
            return Ok(());
        };
        match pdf.name(&stream.dict, b"Subtype") {
            Some(b"Image") => {
                let stencil = matches!(
                    pdf.get(&stream.dict, b"ImageMask"),
                    Some(Object::Boolean(true))
                );
                if !stencil && !self.color_operators {
                    return Ok(());
                }
                if stencil {
                    let paint = self.state().fill.clone();
                    self.process_color(&paint)?;
                }
                let resources = self.resources;
                self.found(Drawn::XObject {
                    identity,
                    stream,
                    resources,
                })
            }
            Some(b"Form") => {
                self.level += 1;
                let result = if self.level > MAX_FORM_LEVEL {
                    Ok(())
                } else if is_transparency_group(pdf, stream) {
                    self.process_group(stream)
                } else if pdf.int(&stream.dict, b"Length", 0) > 0 {
                    let data = pdf.decode(stream)?;
                    let resources = pdf.dict(&stream.dict, b"Resources");
                    self.process_stream(&data, resources, Content::Stream)
                } else {
                    Ok(())
                };
                self.level -= 1;
                result
            }
            _ => Ok(()),
        }
    }

    /// `BeginInlineImage`.
    fn inline_image(&mut self, image: InlineImage) -> Result<(), WriteFailure> {
        if image.data.as_ref().is_none_or(Vec::is_empty) {
            return Ok(());
        }
        let stencil = {
            let pdf = self.pdf;
            let get = |first: &[u8], second: &[u8]| {
                pdf.get(&image.parameters, first)
                    .or_else(|| pdf.get(&image.parameters, second))
            };
            matches!(get(b"IM", b"ImageMask"), Some(Object::Boolean(true)))
        };
        if !stencil && !self.color_operators {
            return Ok(());
        }
        let resources = self.resources;
        self.found(Drawn::Inline { image, resources })
    }
}

/// Whether a text-showing operator shows any glyph: a string of at least one byte.
fn shows_glyphs(name: &str, operands: &[Object]) -> bool {
    let nonempty =
        |object: &Object| matches!(object, Object::String(bytes, _) if !bytes.is_empty());
    match name {
        "TJ" => {
            matches!(operands.first(), Some(Object::Array(items)) if items.iter().any(nonempty))
        }
        "\"" => operands.get(2).is_some_and(nonempty),
        _ => operands.first().is_some_and(nonempty),
    }
}
