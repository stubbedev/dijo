//! PDFBox's `PDFStreamEngine`, as `LegacyPDFStreamEngine` and `PDFTextStripper` run it for text
//! extraction: a page's content stream and the forms it draws, the graphics and text state the
//! text operators change, and each glyph shown placed as a [`TextPosition`] in the article it
//! falls in, with what Tika's tagged writer records of the marked content around it.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use lopdf::{Dictionary, Object};

use super::content::{Lexer, Token};
use super::cos::{as_dict, float_value, text_string, Identity, Pdf};
use super::document::Page;
use super::font::{rectangle, Font};
use super::jfloat;
use super::matrix::Matrix;
use super::position::TextPosition;
use super::structure::Label;
use super::Unported;

/// How deep forms may draw forms before PDFBox skips them.
const MAX_FORM_LEVEL: i32 = 50;

/// `PDTextState`, as text extraction reads it.
#[derive(Debug, Clone)]
struct TextState {
    char_spacing: f32,
    word_spacing: f32,
    horizontal_scaling: f32,
    leading: f32,
    font: Option<Rc<Font>>,
    font_size: f32,
    rise: f32,
}

/// `PDGraphicsState`, as text extraction reads it.
#[derive(Debug, Clone)]
struct GraphicsState {
    ctm: Matrix,
    text: TextState,
    text_matrix: Option<Matrix>,
    text_line_matrix: Option<Matrix>,
}

impl GraphicsState {
    fn new() -> Self {
        Self {
            ctm: Matrix::default(),
            text: TextState {
                char_spacing: 0.0,
                word_spacing: 0.0,
                horizontal_scaling: 100.0,
                leading: 0.0,
                font: None,
                font_size: 0.0,
                rise: 0.0,
            },
            text_matrix: None,
            text_line_matrix: None,
        }
    }
}

/// What a page's content stream shows.
#[derive(Debug, Default)]
pub(super) struct PageContent {
    /// `charactersByArticle`: the glyphs, in the articles the page's beads divide it into.
    pub(super) articles: Vec<Vec<TextPosition>>,
    /// The glyphs shown, and those without a Unicode mapping.
    pub(super) glyphs: i32,
    pub(super) unmapped: i32,
    /// The marked-content identifiers the page's streams open, tagged with their scope.
    pub(super) opened: HashSet<Label>,
    /// The page and the forms with structure parents it draws, in the order first drawn.
    pub(super) scopes: Vec<Identity>,
}

/// The text extraction engine of one document.
pub(super) struct Engine<'p> {
    pdf: &'p Pdf,
    fonts: HashMap<Identity, Rc<Font>>,
    default_font: Option<Rc<Font>>,
    unshared: usize,
    /// Whether glyphs are labelled with their marked content, as Tika's tagged writer does
    /// when the document has a usable structure tree.
    labels: bool,
    pub(super) damaged_font: bool,
    pub(super) non_embedded_font: bool,
}

/// `PDMarkedContent`, as `PDFTextStripper` keeps it.
struct MarkedContent {
    has_actual_text: bool,
}

/// Tika's `MarkedContentFrame`.
struct Frame {
    artifact: bool,
    mcid: i32,
}

/// One page being processed.
struct Run<'p, 'e> {
    engine: &'e mut Engine<'p>,
    pdf: &'p Pdf,
    page: Page<'p>,
    rotation: i32,
    page_box: [f32; 4],
    translate: Option<Matrix>,
    stack: Vec<GraphicsState>,
    resources: Option<&'p Dictionary>,
    level: i32,
    marked: Vec<MarkedContent>,
    actual_text: Option<String>,
    first_actual_text: bool,
    frames: Vec<Frame>,
    scopes: Vec<Identity>,
    current_label: Option<Label>,
    beads: Vec<Option<[f32; 4]>>,
    content: PageContent,
}

impl<'p> Engine<'p> {
    pub(super) fn new(pdf: &'p Pdf, labels: bool) -> Self {
        Self {
            pdf,
            fonts: HashMap::new(),
            default_font: None,
            unshared: 0,
            labels,
            damaged_font: false,
            non_embedded_font: false,
        }
    }

    /// `PDFTextStripper.processPage` up to `writePage`: the page's glyphs by article.
    pub(super) fn process_page(&mut self, page: Page<'p>) -> Result<PageContent, Unported> {
        let pdf = self.pdf;
        let crop = page.crop_box(pdf);
        let translate = (!jfloat::same(crop[0], 0.0) || !jfloat::same(crop[1], 0.0))
            .then(|| Matrix::translation(-crop[0], -crop[1]));
        let beads = beads(pdf, page);
        let mut run = Run {
            engine: self,
            pdf,
            page,
            rotation: page.rotation(pdf),
            page_box: crop,
            translate,
            stack: vec![GraphicsState::new()],
            resources: None,
            level: 0,
            marked: Vec::new(),
            actual_text: None,
            first_actual_text: false,
            frames: Vec::new(),
            scopes: Vec::new(),
            current_label: None,
            content: PageContent {
                articles: vec![Vec::new(); 1 + beads.len() * 2],
                scopes: vec![page.identity],
                ..PageContent::default()
            },
            beads,
        };
        if page.has_contents(pdf) {
            let data = page.contents(pdf)?;
            let resources = page.resources(pdf);
            run.process_stream(&data, resources, Matrix::default(), true)?;
        }
        Ok(run.content)
    }

    /// The font a resource names, created once per font dictionary as PDFBox's resource cache
    /// holds it.
    fn font(&mut self, identity: Identity, dict: &Dictionary) -> Result<Rc<Font>, Unported> {
        if let Some(font) = self.fonts.get(&identity) {
            return Ok(Rc::clone(font));
        }
        let font = Rc::new(Font::load(self.pdf, identity, dict)?);
        self.fonts.insert(identity, Rc::clone(&font));
        Ok(font)
    }

    /// A font PDFBox creates anew each time, outside its resource cache.
    fn unshared_font(&mut self, dict: &Dictionary) -> Result<Rc<Font>, Unported> {
        self.unshared += 1;
        Ok(Rc::new(Font::load(
            self.pdf,
            Identity::Unshared(self.unshared),
            dict,
        )?))
    }

    /// `PDFStreamEngine.getDefaultFont`: Helvetica, for text shown without a font.
    fn default_font(&mut self) -> Result<Rc<Font>, Unported> {
        if let Some(font) = &self.default_font {
            return Ok(Rc::clone(font));
        }
        let mut dict = Dictionary::new();
        dict.set("Type", Object::Name(b"Font".to_vec()));
        dict.set("Subtype", Object::Name(b"Type1".to_vec()));
        dict.set("BaseFont", Object::Name(b"Helvetica".to_vec()));
        dict.set("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()));
        let font = Rc::new(Font::load(self.pdf, Identity::Unshared(0), &dict)?);
        self.default_font = Some(Rc::clone(&font));
        Ok(font)
    }
}

/// `PDFTextStripper.fillBeadRectangles`: the page's thread beads in the page's top-down
/// coordinates, adjusted for its crop box.
fn beads(pdf: &Pdf, page: Page) -> Vec<Option<[f32; 4]>> {
    let Some(array) = pdf.array(page.dict, b"B") else {
        return Vec::new();
    };
    let media = page.media_box(pdf);
    let crop = page.crop_box(pdf);
    array
        .iter()
        .map(|item| {
            let bead = pdf.resolve(item).and_then(as_dict)?;
            let rect = pdf.array(bead, b"R").map(|r| rectangle(pdf, r))?;
            let mut rect = [rect[0], media[3] - rect[3], rect[2], media[3] - rect[1]];
            if !jfloat::same(crop[0], 0.0) || !jfloat::same(crop[1], 0.0) {
                rect = [
                    rect[0] - crop[0],
                    rect[1] - crop[1],
                    rect[2] - crop[0],
                    rect[3] - crop[1],
                ];
            }
            Some(rect)
        })
        .collect()
}

fn number(object: &Object) -> Option<f32> {
    float_value(object)
}

impl<'p> Run<'p, '_> {
    fn state(&mut self) -> &mut GraphicsState {
        self.stack
            .last_mut()
            .unwrap_or_else(|| unreachable!("the graphics stack is never empty"))
    }

    /// `processStream`: a content stream with its resources, its matrix applied to the
    /// graphics state, on a graphics stack of its own.
    fn process_stream(
        &mut self,
        data: &[u8],
        resources: Option<&'p Dictionary>,
        matrix: Matrix,
        page: bool,
    ) -> Result<(), Unported> {
        let parent = self.resources;
        self.resources = match resources {
            Some(resources) => Some(resources),
            None if parent.is_some() => parent,
            None if page => None,
            None => self.page.resources(self.pdf),
        };
        let top = self.state().clone();
        let saved = std::mem::replace(&mut self.stack, vec![top]);
        let result = self
            .state()
            .ctm
            .concatenate(&matrix)
            .and_then(|()| self.process_operators(data));
        self.stack = saved;
        self.resources = parent;
        result
    }

    fn process_operators(&mut self, data: &[u8]) -> Result<(), Unported> {
        let mut lexer = Lexer::new(data);
        let mut operands: Vec<Object> = Vec::new();
        while let Some(token) = lexer.next_token()? {
            match token {
                Token::Operand(operand) => operands.push(operand),
                Token::Operator(operator) => {
                    self.operator(&operator, &operands)?;
                    operands.clear();
                }
            }
        }
        Ok(())
    }

    /// `processOperator`: an operator text extraction handles, with what PDFBox does on its
    /// missing or mistyped operands.
    fn operator(&mut self, name: &str, operands: &[Object]) -> Result<(), Unported> {
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
            "cm" => {
                if let Some(matrix) = six_numbers(operands) {
                    self.state().ctm.concatenate(&matrix)?;
                }
            }
            "BT" => {
                let state = self.state();
                state.text_matrix = Some(Matrix::default());
                state.text_line_matrix = Some(Matrix::default());
            }
            "ET" => {
                let state = self.state();
                state.text_matrix = None;
                state.text_line_matrix = None;
            }
            "Tc" => {
                if let Some(value) = operands.last().and_then(number) {
                    self.state().text.char_spacing = value;
                }
            }
            "Tw" => {
                if let Some(value) = operands.first().and_then(number) {
                    self.state().text.word_spacing = value;
                }
            }
            "Tz" => {
                if let Some(value) = operands.first().and_then(number) {
                    self.state().text.horizontal_scaling = value;
                }
            }
            "TL" => {
                if let Some(value) = operands.first().and_then(number) {
                    self.state().text.leading = value;
                }
            }
            "Ts" => {
                if let Some(value) = operands.first().and_then(number) {
                    self.state().text.rise = value;
                }
            }
            "Tf" => self.set_font(operands)?,
            "Td" => self.move_text(operands)?,
            "TD" => {
                if operands.len() >= 2 {
                    if let Some(y) = number(&operands[1]) {
                        self.operator("TL", &[Object::Real(-y)])?;
                        self.operator("Td", operands)?;
                    }
                }
            }
            "T*" => {
                let leading = self.state().text.leading;
                self.operator("Td", &[Object::Real(0.0), Object::Real(-leading)])?;
            }
            "Tm" => {
                if let Some(matrix) = six_numbers(operands) {
                    let state = self.state();
                    state.text_matrix = Some(matrix);
                    state.text_line_matrix = Some(matrix);
                }
            }
            "Tj" => {
                if let Some(Object::String(bytes, _)) = operands.first() {
                    if self.state().text_matrix.is_some() {
                        self.show_text(bytes)?;
                    }
                }
            }
            "TJ" => {
                if let Some(Object::Array(items)) = operands.first() {
                    if self.state().text_matrix.is_some() {
                        self.show_text_strings(items)?;
                    }
                }
            }
            "'" => {
                self.operator("T*", &[])?;
                self.operator("Tj", operands)?;
            }
            "\"" => {
                if operands.len() >= 3 {
                    self.operator("Tw", &operands[0..1])?;
                    self.operator("Tc", &operands[1..2])?;
                    self.operator("'", &operands[2..3])?;
                }
            }
            "gs" => self.graphics_state_parameters(operands)?,
            "Do" => self.draw_object(operands)?,
            "BDC" => self.begin_marked_content_with_properties(operands),
            "BMC" => {
                let tag = operands
                    .iter()
                    .filter_map(|operand| match operand {
                        Object::Name(name) => Some(name.as_slice()),
                        _ => None,
                    })
                    .next_back();
                self.begin_marked_content(tag, None);
            }
            "EMC" => self.end_marked_content(),
            _ => {}
        }
        Ok(())
    }

    /// `SetFontAndSize`.
    fn set_font(&mut self, operands: &[Object]) -> Result<(), Unported> {
        if operands.len() < 2 {
            return Ok(());
        }
        let (Object::Name(name), Some(size)) = (&operands[0], number(&operands[1])) else {
            return Ok(());
        };
        self.state().text.font_size = size;
        let pdf = self.pdf;
        let font = match self
            .resources
            .and_then(|resources| pdf.dict(resources, b"Font"))
            .and_then(|fonts| pdf.get_identified(fonts, name))
        {
            Some((identity, object)) => match as_dict(object) {
                Some(dict) => Some(self.engine.font(identity, dict)?),
                None => None,
            },
            None => None,
        };
        self.state().text.font = font;
        Ok(())
    }

    /// `MoveText`.
    fn move_text(&mut self, operands: &[Object]) -> Result<(), Unported> {
        if operands.len() < 2 {
            return Ok(());
        }
        let state = self.state();
        let Some(line) = state.text_line_matrix.as_mut() else {
            return Ok(());
        };
        let (Some(x), Some(y)) = (number(&operands[0]), number(&operands[1])) else {
            return Ok(());
        };
        line.concatenate(&Matrix::translation(x, y))?;
        let line = *line;
        state.text_matrix = Some(line);
        Ok(())
    }

    /// `SetGraphicsStateParameters`: the font of an `ExtGState`.
    fn graphics_state_parameters(&mut self, operands: &[Object]) -> Result<(), Unported> {
        let Some(Object::Name(name)) = operands.first() else {
            return Ok(());
        };
        let pdf = self.pdf;
        let Some(state) = self
            .resources
            .and_then(|resources| pdf.dict(resources, b"ExtGState"))
            .and_then(|states| pdf.dict(states, name))
        else {
            return Ok(());
        };
        for (key, _) in state.iter() {
            if key != b"Font" {
                continue;
            }
            let Some(setting) = pdf.array(state, b"Font") else {
                continue;
            };
            let font = match setting
                .first()
                .and_then(|item| pdf.resolve(item))
                .and_then(as_dict)
            {
                Some(dict) => Some(self.engine.unshared_font(dict)?),
                None => None,
            };
            let size = match setting.get(1) {
                Some(Object::Integer(_) | Object::Real(_)) => setting.get(1).and_then(number),
                _ => None,
            }
            .ok_or_else(|| {
                Unported(
                    "a graphics state font without a direct size, which PDFBox fails on".to_owned(),
                )
            })?;
            let text = &mut self.state().text;
            text.font = font;
            text.font_size = size;
        }
        Ok(())
    }

    /// `DrawObject`: a form, drawn with its resources and matrix; images are skipped.
    fn draw_object(&mut self, operands: &[Object]) -> Result<(), Unported> {
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
        let subtype = pdf.name_as_string(&stream.dict, b"Subtype");
        if subtype.as_deref() != Some("Form") {
            return Ok(());
        }
        self.level += 1;
        let result = if self.level > MAX_FORM_LEVEL {
            Ok(())
        } else {
            self.show_form(identity, stream)
        };
        self.level -= 1;
        result
    }

    /// `showForm` and `showTransparencyGroup`, with Tika's tagged writer's scope of a form
    /// with structure parents.
    fn show_form(&mut self, identity: Identity, stream: &'p lopdf::Stream) -> Result<(), Unported> {
        let pdf = self.pdf;
        let transparency = pdf
            .dict(&stream.dict, b"Group")
            .and_then(|group| pdf.name(group, b"S"))
            == Some(b"Transparency");
        if !transparency && pdf.int(&stream.dict, b"Length", 0) <= 0 {
            return Ok(());
        }
        let scoped = self.engine.labels && pdf.int(&stream.dict, b"StructParents", -1) >= 0;
        if scoped {
            self.scopes.push(identity);
            if !self.content.scopes.contains(&identity) {
                self.content.scopes.push(identity);
            }
            self.current_label = None;
        }
        let data = pdf
            .decode(stream)
            .map_err(|unported| Unported(format!("a form's content stream: {}", unported.0)));
        let result = data.and_then(|data| {
            let matrix = form_matrix(pdf, &stream.dict);
            let resources = pdf.dict(&stream.dict, b"Resources");
            self.process_stream(&data, resources, matrix, false)
        });
        if scoped {
            self.scopes.pop();
            self.current_label = None;
        }
        result
    }

    /// `BeginMarkedContentSequenceWithProperties`.
    fn begin_marked_content_with_properties(&mut self, operands: &[Object]) {
        if operands.len() < 2 {
            return;
        }
        let Object::Name(tag) = &operands[0] else {
            return;
        };
        let pdf = self.pdf;
        let properties = match &operands[1] {
            Object::Name(name) => self
                .resources
                .and_then(|resources| pdf.dict(resources, b"Properties"))
                .and_then(|properties| pdf.dict(properties, name)),
            Object::Dictionary(dict) => Some(dict),
            _ => None,
        };
        let Some(properties) = properties else {
            return;
        };
        let tag = tag.clone();
        self.begin_marked_content(Some(&tag), Some(properties));
    }

    /// `beginMarkedContentSequence`, as Tika's tagged writer and `PDFTextStripper` both take
    /// it.
    fn begin_marked_content(&mut self, tag: Option<&[u8]>, properties: Option<&Dictionary>) {
        let pdf = self.pdf;
        let mcid = properties.map_or(-1, |p| pdf.int(p, b"MCID", -1));
        if self.engine.labels {
            self.frames.push(Frame {
                artifact: tag == Some(b"Artifact"),
                mcid,
            });
            self.current_label = None;
            if mcid >= 0 {
                let scope = self.scopes.last().copied().unwrap_or(self.page.identity);
                self.content.opened.insert(Label::Tagged { scope, mcid });
            }
        }
        let actual_text = properties.and_then(|p| match pdf.get(p, b"ActualText") {
            Some(Object::String(bytes, _)) => Some(text_string(bytes)),
            _ => None,
        });
        self.marked.push(MarkedContent {
            has_actual_text: actual_text.is_some(),
        });
        self.actual_text = actual_text.map(|text| text.replace('\u{AD}', ""));
        if self.actual_text.is_some() {
            self.first_actual_text = true;
        }
    }

    /// `endMarkedContentSequence`, as Tika's tagged writer and `PDFTextStripper` both take it.
    fn end_marked_content(&mut self) {
        if self.engine.labels {
            self.frames.pop();
            self.current_label = None;
        }
        if let Some(marked) = self.marked.pop() {
            if marked.has_actual_text {
                self.actual_text = None;
            }
        }
    }

    /// Tika's `PDFMarkedContent2XHTML.label`: what the glyphs drawn now are drawn under.
    fn label(&mut self) -> Label {
        if let Some(label) = self.current_label {
            return label;
        }
        let mut mcid = -1;
        let mut artifact = false;
        for frame in self.frames.iter().rev() {
            if frame.artifact {
                artifact = true;
                break;
            }
            if mcid < 0 && frame.mcid >= 0 {
                mcid = frame.mcid;
            }
        }
        let label = if artifact {
            Label::Artifact
        } else if mcid < 0 {
            Label::Untagged
        } else {
            let scope = self.scopes.last().copied().unwrap_or(self.page.identity);
            Label::Tagged { scope, mcid }
        };
        self.current_label = Some(label);
        label
    }

    /// `showTextStrings`: the strings of `TJ` with the adjustments between them.
    fn show_text_strings(&mut self, items: &[Object]) -> Result<(), Unported> {
        let text = &self.state().text;
        let font_size = text.font_size;
        let horizontal_scaling = text.horizontal_scaling / 100.0;
        let vertical = text.font.as_ref().is_some_and(|font| font.is_vertical());
        for item in items {
            match item {
                Object::Integer(_) | Object::Real(_) => {
                    let adjustment = number(item).unwrap_or_default();
                    let (tx, ty) = if vertical {
                        (0.0, -adjustment / 1000.0 * font_size)
                    } else {
                        (-adjustment / 1000.0 * font_size * horizontal_scaling, 0.0)
                    };
                    if let Some(matrix) = self.state().text_matrix.as_mut() {
                        matrix.translate(tx, ty)?;
                    }
                }
                Object::String(bytes, _) => {
                    self.show_text(bytes)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// `showText`: each glyph of a string shown and the text matrix advanced past it.
    fn show_text(&mut self, string: &[u8]) -> Result<(), Unported> {
        let font = match self.state().text.font.clone() {
            Some(font) => font,
            None => self.engine.default_font()?,
        };
        let text = self.state().text.clone();
        let horizontal_scaling = text.horizontal_scaling / 100.0;
        let parameters = Matrix::new(
            text.font_size * horizontal_scaling,
            0.0,
            0.0,
            text.font_size,
            0.0,
            text.rise,
        );
        let mut rest = string;
        while !rest.is_empty() {
            let code = font.read_code(rest)?;
            let length = code.length;
            rest = &rest[length.min(rest.len())..];
            let word_spacing = if length == 1 && code.code == 32 {
                text.word_spacing
            } else {
                0.0
            };
            let state = self.state();
            let text_matrix = state.text_matrix.unwrap_or_default();
            let mut rendering = parameters.multiply(&text_matrix)?.multiply(&state.ctm)?;
            if font.is_vertical() {
                let (x, y) = font.position_vector(code.code)?;
                rendering.translate(x, y)?;
            }
            let displacement = font.displacement(code.code)?;
            self.show_glyph(&rendering, &font, code.code, displacement)?;
            let (tx, ty) = if font.is_vertical() {
                (
                    0.0,
                    displacement.1 * text.font_size + text.char_spacing + word_spacing,
                )
            } else {
                (
                    (displacement.0 * text.font_size + text.char_spacing + word_spacing)
                        * horizontal_scaling,
                    0.0,
                )
            };
            if let Some(matrix) = self.state().text_matrix.as_mut() {
                matrix.translate(tx, ty)?;
            }
            if length == 0 {
                break;
            }
        }
        Ok(())
    }

    /// `LegacyPDFStreamEngine.showGlyph` and `AbstractPDF2XHTML.showGlyph`: the glyph placed,
    /// and counted.
    fn show_glyph(
        &mut self,
        rendering: &Matrix,
        font: &Rc<Font>,
        code: u32,
        displacement: (f32, f32),
    ) -> Result<(), Unported> {
        self.legacy_show_glyph(rendering, font, code, displacement)?;
        let unicode = font.to_unicode(code, false)?;
        if unicode.as_deref().is_none_or(str::is_empty) {
            self.content.unmapped += 1;
        }
        self.content.glyphs += 1;
        if font.is_damaged() {
            self.engine.damaged_font = true;
        }
        if !font.is_embedded() {
            self.engine.non_embedded_font = true;
        }
        Ok(())
    }

    fn legacy_show_glyph(
        &mut self,
        rendering: &Matrix,
        font: &Rc<Font>,
        code: u32,
        displacement: (f32, f32),
    ) -> Result<(), Unported> {
        let state = self.state();
        let font_size = state.text.font_size;
        let horizontal_scaling = state.text.horizontal_scaling / 100.0;
        let text_matrix = state.text_matrix.unwrap_or_default();
        let ctm = state.ctm;
        let mut displacement_x = displacement.0;
        if font.is_vertical() {
            displacement_x = font.width(code)? / 1000.0;
            if let Some(units) = font.true_type_units_per_em()? {
                if units != 1000 {
                    displacement_x *= 1000.0 / f32::from(units);
                }
            }
        }
        let tx = displacement_x * font_size * horizontal_scaling;
        let ty = displacement.1 * font_size;
        let next = Matrix::translation(tx, ty)
            .multiply(&text_matrix)?
            .multiply(&ctm)?;
        let mut next_x = next.translate_x();
        let mut next_y = next.translate_y();
        let dx_display = next_x - rendering.translate_x();
        let font_height = font.height()?;
        let dy_display = font_height * rendering.scaling_factor_y();
        let glyph_to_text = match font.type3_matrix() {
            Some(matrix) => matrix.scale_x(),
            None => 1.0 / 1000.0,
        };
        let mut space_width_text = font.space_width()? * glyph_to_text;
        if jfloat::same(space_width_text, 0.0) {
            space_width_text = font.average_width() * glyph_to_text;
            space_width_text *= 0.80;
        }
        if jfloat::same(space_width_text, 0.0) {
            space_width_text = 1.0;
        }
        let space_width_display = space_width_text * rendering.scaling_factor_x();
        let unicode = match font.to_unicode(code, true)? {
            Some(unicode) => unicode,
            None if font.is_simple() => match char::from_u32(code & 0xFFFF) {
                Some(c) => c.to_string(),
                None => {
                    return Err(Unported(
                        "a glyph coerced to half of a UTF-16 surrogate pair".to_owned(),
                    ))
                }
            },
            None => return Ok(()),
        };
        let translated = match &self.translate {
            None => *rendering,
            Some(translate) => {
                next_x -= self.page_box[0];
                next_y -= self.page_box[1];
                rendering.multiply(translate)?
            }
        };
        let label = if self.engine.labels {
            self.label()
        } else {
            Label::Untagged
        };
        let position = TextPosition::new(
            self.rotation,
            self.page_box[2] - self.page_box[0],
            self.page_box[3] - self.page_box[1],
            translated,
            next_x,
            next_y,
            dy_display.abs(),
            dx_display,
            space_width_display.abs(),
            unicode,
            Rc::clone(font),
            font_size,
            (font_size * text_matrix.scaling_factor_x()) as i32,
            label,
        );
        self.process_text_position(position);
        Ok(())
    }

    /// `PDFTextStripper.processTextPosition`, with duplicate overlapping text kept, as Tika
    /// keeps it: the glyph's actual text, its article, and diacritics merged into the glyph
    /// they sit on.
    fn process_text_position(&mut self, mut text: TextPosition) {
        if let Some(actual) = &self.actual_text {
            if self.first_actual_text {
                text.set_unicode(actual.clone());
                self.first_actual_text = false;
            } else {
                text.set_unicode(String::new());
            }
        }
        let x = text.x();
        let y = text.y();
        let mut found = None;
        let mut left_and_above = None;
        let mut left = None;
        let mut above = None;
        for (index, bead) in self.beads.iter().enumerate() {
            if found.is_some() {
                break;
            }
            match bead {
                Some([llx, lly, urx, ury]) => {
                    if x >= *llx && x <= *urx && y >= *lly && y <= *ury {
                        found = Some(index * 2 + 1);
                    } else if (x < *llx || y < *ury) && left_and_above.is_none() {
                        left_and_above = Some(index * 2);
                    } else if x < *llx && left.is_none() {
                        left = Some(index * 2);
                    } else if y < *ury && above.is_none() {
                        above = Some(index * 2);
                    }
                }
                None => found = Some(0),
            }
        }
        let articles = &mut self.content.articles;
        let index = if self.beads.is_empty() {
            0
        } else {
            found
                .or(left_and_above)
                .or(left)
                .or(above)
                .unwrap_or(articles.len() - 1)
        };
        let list = &mut articles[index];
        match list.last_mut() {
            None => list.push(text),
            Some(previous) => {
                if text.is_diacritic() && previous.contains(&text) {
                    previous.merge_diacritic(&text);
                } else if previous.is_diacritic() && text.contains(previous) {
                    let previous = list.pop().unwrap_or_else(|| unreachable!());
                    text.merge_diacritic(&previous);
                    list.push(text);
                } else {
                    list.push(text);
                }
            }
        }
    }
}

/// Six numbers, as `cm` and `Tm` take them, all operands numbers.
fn six_numbers(operands: &[Object]) -> Option<Matrix> {
    if operands.len() < 6 || !operands.iter().all(|o| number(o).is_some()) {
        return None;
    }
    let n: Vec<f32> = operands.iter().take(6).filter_map(number).collect();
    Some(Matrix::new(n[0], n[1], n[2], n[3], n[4], n[5]))
}

/// `PDFormXObject.getMatrix`: `/Matrix` when it holds six numbers, else identity.
fn form_matrix(pdf: &Pdf, dict: &Dictionary) -> Matrix {
    let Some(array) = pdf.get(dict, b"Matrix") else {
        return Matrix::default();
    };
    let Object::Array(array) = array else {
        return Matrix::default();
    };
    if array.len() < 6 {
        return Matrix::default();
    }
    let values: Option<Vec<f32>> = array
        .iter()
        .take(6)
        .map(|item| pdf.resolve(item).and_then(float_value))
        .collect();
    match values {
        Some(n) => Matrix::new(n[0], n[1], n[2], n[3], n[4], n[5]),
        None => Matrix::default(),
    }
}
