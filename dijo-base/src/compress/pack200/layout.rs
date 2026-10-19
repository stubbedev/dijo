//! Attribute layouts (JSR 200 sections 5.5.2 and 5.5.3): the little language in which a layout
//! declares the bands of an attribute, the reading of those bands for all the attributes of a
//! layout, and the reading of each attribute from them into [`Piece`]s, whose constant
//! references and bytecode indexes are resolved once its class file is written.

use super::coding::{Bands, Bhsd, BCI5, BRANCH5, BYTE1, SIGNED5, UNSIGNED5};
use super::pool::{index, ClassRef, Key, Pool, Text};
use super::{malformed, Result, Undecoded};

/// The deepest attribute layouts nest by calls back into callables, deeper than any metadata
/// nests in practice, which keeps a layout that calls itself without reading anything from
/// recursing without end.
const MAX_DEPTH: usize = 256;

/// What an integral element stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IntKind {
    /// A number or flags, as transmitted.
    Plain,
    /// 'P': a bytecode index, transmitted renumbered.
    Bci,
    /// 'PO': a bytecode index, transmitted as the difference of its renumbering from that of
    /// the bytecode index before it.
    BciAfter,
    /// 'O': the difference of two bytecode indexes, the one before it and another, transmitted
    /// as the difference of their renumberings.
    Offset,
}

/// An integral element: what it stores, how many bytes it stores it in (none for 'V'), and
/// whether it is signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Int {
    kind: IntKind,
    size: u8,
    signed: bool,
}

impl Int {
    /// The primary coding of the band of the element.
    fn coding(self) -> Bhsd {
        match self.kind {
            IntKind::Bci => BCI5,
            IntKind::BciAfter | IntKind::Offset => BRANCH5,
            IntKind::Plain if self.signed => SIGNED5,
            IntKind::Plain if self.size == 1 => BYTE1,
            IntKind::Plain => UNSIGNED5,
        }
    }
}

/// The pool a reference element refers into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefKind {
    Int,
    Long,
    Float,
    Double,
    String,
    /// 'KQ': the pool the type of the field the attribute is on selects.
    FieldConstant,
    Class,
    Signature,
    Descr,
    Field,
    Method,
    Imethod,
    Utf8,
    /// 'RQ': `cp_All`.
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Element {
    Int {
        band: usize,
        int: Int,
    },
    Replication {
        band: usize,
        count: Int,
        body: Vec<Element>,
    },
    Union {
        band: usize,
        tag: Int,
        /// The tags of each case, as inclusive ranges, and its body.
        cases: Vec<(Vec<(i32, i32)>, Vec<Element>)>,
        default: Vec<Element>,
    },
    Call {
        callable: usize,
        backward: bool,
    },
    Ref {
        band: usize,
        kind: RefKind,
        nullable: bool,
        size: u8,
    },
}

/// A value of an attribute, as its layout stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Piece {
    /// A number, in `size` bytes.
    Int { size: u8, value: i32 },
    /// A reference to a constant, or to none, in `size` bytes.
    Ref { size: u8, key: Option<Key> },
    /// A bytecode index or offset of the given kind, as transmitted, in `size` bytes.
    Bci {
        size: u8,
        kind: IntKind,
        value: i32,
    },
}

/// A parsed attribute layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Layout {
    callables: Vec<Vec<Element>>,
    /// Whether each callable is the callee of a backward call.
    backward: Vec<bool>,
    bands: usize,
}

impl Layout {
    /// The layout spelled `spelling`.
    pub(super) fn parse(spelling: &[u16]) -> Result<Layout> {
        let text: Vec<u8> = spelling
            .iter()
            .filter(|&&c| !matches!(c, 0x20 | 0x09 | 0x0A | 0x0D))
            .map(|&c| u8::try_from(c).ok().filter(u8::is_ascii))
            .collect::<Option<_>>()
            .ok_or_else(|| malformed("an attribute layout holds a character beyond ASCII"))?;
        let mut parser = Parser {
            text: &text,
            at: 0,
            bands: 0,
            calls: Vec::new(),
        };
        let mut callables = Vec::new();
        if text.first() == Some(&b'[') {
            while parser.at < text.len() {
                parser.expect(b'[')?;
                let body = parser.body(callables.len())?;
                parser.expect(b']')?;
                callables.push(body);
            }
        } else {
            let body = parser.body(0)?;
            if parser.at != text.len() {
                return Err(parser.error());
            }
            callables.push(body);
        }
        let mut backward = vec![false; callables.len()];
        for &(callable, backward_call) in &parser.calls {
            if callable >= callables.len() {
                return Err(malformed("an attribute layout calls a callable it lacks"));
            }
            backward[callable] |= backward_call;
        }
        Ok(Layout {
            callables,
            backward,
            bands: parser.bands,
        })
    }

    /// The number of its callables that are called back into, each of which has a count of
    /// calls in the `*_attr_calls` band of a layout that is used.
    pub(super) fn backward_callables(&self) -> usize {
        self.backward.iter().filter(|&&backward| backward).count()
    }

    /// The bands of the `occurrences` attributes of the layout, read in its definition order,
    /// `calls` giving the counts of calls back into its callables.
    pub(super) fn read<'l>(
        &'l self,
        bands: &mut Bands,
        occurrences: usize,
        calls: &mut impl Iterator<Item = usize>,
    ) -> Result<Reading<'l>> {
        let mut sizing = Sizing {
            bands,
            values: vec![Vec::new(); self.bands],
            invocations: vec![0; self.callables.len()],
        };
        let mut backward_left = vec![0; self.callables.len()];
        if let Some(first) = sizing.invocations.first_mut() {
            *first = occurrences;
        }
        for (at, callable) in self.callables.iter().enumerate() {
            if self.backward[at] {
                let count = calls
                    .next()
                    .ok_or_else(|| malformed("an attribute layout lacks a call count"))?;
                backward_left[at] = count;
                sizing.invocations[at] = sizing.invocations[at]
                    .checked_add(count)
                    .ok_or_else(|| malformed("the calls of an attribute layout overflow"))?;
            }
            let invocations = sizing.invocations[at];
            sizing.elements(callable, invocations)?;
        }
        let positions = vec![0; self.bands];
        Ok(Reading {
            layout: self,
            values: sizing.values,
            positions,
            backward_left,
        })
    }
}

/// The parsing of a layout.
struct Parser<'t> {
    text: &'t [u8],
    at: usize,
    bands: usize,
    /// The callee of each call and whether the call is backward.
    calls: Vec<(usize, bool)>,
}

impl Parser<'_> {
    fn error(&self) -> Undecoded {
        malformed(format!(
            "the attribute layout {:?} is malformed at {}",
            String::from_utf8_lossy(self.text),
            self.at
        ))
    }

    fn peek(&self) -> Option<u8> {
        self.text.get(self.at).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        let eaten = self.peek() == Some(c);
        if eaten {
            self.at += 1;
        }
        eaten
    }

    fn expect(&mut self, c: u8) -> Result<()> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(self.error())
        }
    }

    fn band(&mut self) -> usize {
        self.bands += 1;
        self.bands - 1
    }

    /// The elements up to a closing bracket or the end, in the callable `callable`.
    fn body(&mut self, callable: usize) -> Result<Vec<Element>> {
        let mut elements = Vec::new();
        while let Some(c) = self.peek() {
            if c == b']' {
                break;
            }
            elements.push(self.element(callable)?);
        }
        Ok(elements)
    }

    /// A 'B', 'H', 'I' or 'V', the size of an integer.
    fn size(&mut self) -> Result<u8> {
        let size = match self.peek() {
            Some(b'B') => 1,
            Some(b'H') => 2,
            Some(b'I') => 4,
            Some(b'V') => 0,
            _ => return Err(self.error()),
        };
        self.at += 1;
        Ok(size)
    }

    /// An unsigned or signed integer: its signedness and size.
    fn any_int(&mut self) -> Result<(bool, u8)> {
        let signed = self.eat(b'S');
        Ok((signed, self.size()?))
    }

    fn plain(&mut self) -> Result<Int> {
        let (signed, size) = self.any_int()?;
        Ok(Int {
            kind: IntKind::Plain,
            size,
            signed,
        })
    }

    /// A decimal numeral, truncated to 32 bits.
    fn numeral(&mut self) -> Result<i32> {
        let negative = self.eat(b'-');
        let start = self.at;
        let mut value = 0u32;
        while let Some(digit @ b'0'..=b'9') = self.peek() {
            value = value.wrapping_mul(10).wrapping_add(u32::from(digit - b'0'));
            self.at += 1;
        }
        if self.at == start {
            return Err(self.error());
        }
        let value = value as i32;
        Ok(if negative { value.wrapping_neg() } else { value })
    }

    fn element(&mut self, callable: usize) -> Result<Element> {
        let c = self.peek().ok_or_else(|| self.error())?;
        Ok(match c {
            b'B' | b'H' | b'I' | b'V' | b'S' => Element::Int {
                int: self.plain()?,
                band: self.band(),
            },
            b'F' => {
                self.at += 1;
                Element::Int {
                    int: Int {
                        kind: IntKind::Plain,
                        size: self.size()?,
                        signed: false,
                    },
                    band: self.band(),
                }
            }
            b'P' => {
                self.at += 1;
                let kind = if self.eat(b'O') {
                    IntKind::BciAfter
                } else {
                    IntKind::Bci
                };
                Element::Int {
                    int: Int {
                        kind,
                        size: self.size()?,
                        signed: false,
                    },
                    band: self.band(),
                }
            }
            b'O' => {
                self.at += 1;
                let (signed, size) = self.any_int()?;
                Element::Int {
                    int: Int {
                        kind: IntKind::Offset,
                        size,
                        signed,
                    },
                    band: self.band(),
                }
            }
            b'N' => {
                self.at += 1;
                let count = Int {
                    kind: IntKind::Plain,
                    size: self.size()?,
                    signed: false,
                };
                let band = self.band();
                self.expect(b'[')?;
                let body = self.body(callable)?;
                self.expect(b']')?;
                Element::Replication { band, count, body }
            }
            b'T' => {
                self.at += 1;
                let tag = self.plain()?;
                let band = self.band();
                let mut cases = Vec::new();
                loop {
                    self.expect(b'(')?;
                    if self.eat(b')') {
                        self.expect(b'[')?;
                        let default = self.body(callable)?;
                        self.expect(b']')?;
                        break Element::Union {
                            band,
                            tag,
                            cases,
                            default,
                        };
                    }
                    let mut tags = Vec::new();
                    loop {
                        let low = self.numeral()?;
                        let high = if self.eat(b'-') { self.numeral()? } else { low };
                        tags.push((low, high));
                        if !self.eat(b',') {
                            break;
                        }
                    }
                    self.expect(b')')?;
                    self.expect(b'[')?;
                    let body = self.body(callable)?;
                    self.expect(b']')?;
                    cases.push((tags, body));
                }
            }
            b'(' => {
                self.at += 1;
                let relative = self.numeral()?;
                self.expect(b')')?;
                let callee = i64::try_from(callable).expect("a small index") + i64::from(relative);
                let callable = usize::try_from(callee).map_err(|_| self.error())?;
                let backward = relative <= 0;
                self.calls.push((callable, backward));
                Element::Call { callable, backward }
            }
            b'K' | b'R' => {
                self.at += 1;
                let kind = match (c, self.peek()) {
                    (b'K', Some(b'I')) => RefKind::Int,
                    (b'K', Some(b'J')) => RefKind::Long,
                    (b'K', Some(b'F')) => RefKind::Float,
                    (b'K', Some(b'D')) => RefKind::Double,
                    (b'K', Some(b'S')) => RefKind::String,
                    (b'K', Some(b'Q')) => RefKind::FieldConstant,
                    (b'R', Some(b'C')) => RefKind::Class,
                    (b'R', Some(b'S')) => RefKind::Signature,
                    (b'R', Some(b'D')) => RefKind::Descr,
                    (b'R', Some(b'F')) => RefKind::Field,
                    (b'R', Some(b'M')) => RefKind::Method,
                    (b'R', Some(b'I')) => RefKind::Imethod,
                    (b'R', Some(b'U')) => RefKind::Utf8,
                    (b'R', Some(b'Q')) => RefKind::Any,
                    _ => return Err(self.error()),
                };
                self.at += 1;
                let nullable = self.eat(b'N');
                Element::Ref {
                    kind,
                    nullable,
                    size: self.size()?,
                    band: self.band(),
                }
            }
            _ => return Err(self.error()),
        })
    }
}

/// The reading of the bands of a layout.
struct Sizing<'b, 'd> {
    bands: &'b mut Bands<'d>,
    values: Vec<Vec<i32>>,
    /// How many times each callable is entered.
    invocations: Vec<usize>,
}

impl Sizing<'_, '_> {
    /// Reads the bands of `elements`, entered `count` times.
    fn elements(&mut self, elements: &[Element], count: usize) -> Result<()> {
        for element in elements {
            match element {
                Element::Int { band, int } => {
                    self.values[*band] = self.bands.band(int.coding(), count)?;
                }
                Element::Ref { band, .. } => {
                    self.values[*band] = self.bands.band(UNSIGNED5, count)?;
                }
                Element::Replication { band, count: int, body } => {
                    let counts = self.bands.band(int.coding(), count)?;
                    let mut total = 0usize;
                    for &replicated in &counts {
                        let replicated = usize::try_from(replicated).map_err(|_| {
                            malformed(format!("an attribute repeats {replicated} times"))
                        })?;
                        total = total
                            .checked_add(replicated)
                            .ok_or_else(|| malformed("an attribute repeats too often"))?;
                    }
                    if !body.is_empty() && total > self.bands.input.remaining() {
                        return Err(malformed(
                            "an attribute repeats more often than its bands could tell",
                        ));
                    }
                    self.values[*band] = counts;
                    self.elements(body, total)?;
                }
                Element::Union {
                    band,
                    tag,
                    cases,
                    default,
                } => {
                    let tags = self.bands.band(tag.coding(), count)?;
                    for (labels, body) in cases {
                        let matching = tags.iter().filter(|&&tag| matches(labels, tag)).count();
                        self.elements(body, matching)?;
                    }
                    let unmatched = tags
                        .iter()
                        .filter(|&&tag| !cases.iter().any(|(labels, _)| matches(labels, tag)))
                        .count();
                    self.elements(default, unmatched)?;
                    self.values[*band] = tags;
                }
                Element::Call { callable, backward } => {
                    if !backward {
                        let invocations = self
                            .invocations
                            .get_mut(*callable)
                            .ok_or_else(|| malformed("an attribute layout calls past its end"))?;
                        *invocations = invocations
                            .checked_add(count)
                            .ok_or_else(|| malformed("the calls of a layout overflow"))?;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Whether `tag` is among the inclusive ranges `labels`.
fn matches(labels: &[(i32, i32)], tag: i32) -> bool {
    labels.iter().any(|&(low, high)| (low..=high).contains(&tag))
}

/// The bands of a layout, read, and how far each has been taken.
#[derive(Debug)]
pub(super) struct Reading<'l> {
    layout: &'l Layout,
    values: Vec<Vec<i32>>,
    positions: Vec<usize>,
    backward_left: Vec<usize>,
}

impl Reading<'_> {
    /// The next attribute of the layout, its 'KQ' references selected by `field_type`, the type
    /// of the field it is on.
    pub(super) fn next(&mut self, pool: &Pool, field_type: Option<&[u16]>) -> Result<Vec<Piece>> {
        let mut pieces = Vec::new();
        let layout = self.layout;
        if let Some(first) = layout.callables.first() {
            self.walk(first, pool, field_type, &mut pieces, 0)?;
        }
        Ok(pieces)
    }

    fn value(&mut self, band: usize) -> Result<i32> {
        let position = self.positions[band];
        let value = *self.values[band]
            .get(position)
            .ok_or_else(|| malformed("an attribute band runs out"))?;
        self.positions[band] = position + 1;
        Ok(value)
    }

    fn walk(
        &mut self,
        elements: &[Element],
        pool: &Pool,
        field_type: Option<&[u16]>,
        pieces: &mut Vec<Piece>,
        depth: usize,
    ) -> Result<()> {
        let layout = self.layout;
        for element in elements {
            match element {
                Element::Int { band, int } => {
                    let value = self.value(*band)?;
                    pieces.push(match int.kind {
                        IntKind::Plain => Piece::Int {
                            size: int.size,
                            value,
                        },
                        kind => Piece::Bci {
                            size: int.size,
                            kind,
                            value,
                        },
                    });
                }
                Element::Replication { band, count, body } => {
                    let value = self.value(*band)?;
                    pieces.push(Piece::Int {
                        size: count.size,
                        value,
                    });
                    if !body.is_empty() {
                        for _ in 0..value {
                            self.walk(body, pool, field_type, pieces, depth)?;
                        }
                    }
                }
                Element::Union {
                    band,
                    tag,
                    cases,
                    default,
                } => {
                    let value = self.value(*band)?;
                    pieces.push(Piece::Int {
                        size: tag.size,
                        value,
                    });
                    let body = cases
                        .iter()
                        .find(|(labels, _)| matches(labels, value))
                        .map_or(default, |(_, body)| body);
                    self.walk(body, pool, field_type, pieces, depth)?;
                }
                Element::Call { callable, backward } => {
                    if *backward {
                        let left = &mut self.backward_left[*callable];
                        if *left == 0 || depth == MAX_DEPTH {
                            return Err(malformed(
                                "an attribute calls back into its layout more than counted",
                            ));
                        }
                        *left -= 1;
                    }
                    let callee = &layout.callables[*callable];
                    self.walk(callee, pool, field_type, pieces, depth + 1)?;
                }
                Element::Ref {
                    band,
                    kind,
                    nullable,
                    size,
                } => {
                    let value = self.value(*band)?;
                    let target = if *nullable {
                        value.checked_sub(1).filter(|_| value != 0)
                    } else {
                        Some(value).filter(|&value| value != -1)
                    };
                    let key = match target {
                        Some(target) => Some(reference(pool, *kind, target, field_type)?),
                        None => None,
                    };
                    pieces.push(Piece::Ref { size: *size, key });
                }
            }
        }
        Ok(())
    }
}

/// The constant of the pool `kind` at `value`.
fn reference(pool: &Pool, kind: RefKind, value: i32, field_type: Option<&[u16]>) -> Result<Key> {
    Ok(match kind {
        RefKind::Int => Key::Int(index(value, pool.ints.len(), "cp_Int")?),
        RefKind::Long => Key::Long(index(value, pool.longs.len(), "cp_Long")?),
        RefKind::Float => Key::Float(index(value, pool.floats.len(), "cp_Float")?),
        RefKind::Double => Key::Double(index(value, pool.doubles.len(), "cp_Double")?),
        RefKind::String => Key::String(index(value, pool.strings.len(), "cp_String")?),
        RefKind::FieldConstant => {
            let field_type = field_type.unwrap_or_default();
            let kind = match field_type {
                [c] if b"BSCZI".iter().any(|&t| *c == u16::from(t)) => RefKind::Int,
                [c] if *c == u16::from(b'J') => RefKind::Long,
                [c] if *c == u16::from(b'F') => RefKind::Float,
                [c] if *c == u16::from(b'D') => RefKind::Double,
                _ if field_type.iter().copied().eq("Ljava/lang/String;".encode_utf16()) => {
                    RefKind::String
                }
                _ if field_type.iter().copied().eq("Ljava/lang/Class;".encode_utf16()) => {
                    RefKind::Class
                }
                _ => {
                    return Err(malformed(format!(
                        "a constant value of a field of type {}",
                        String::from_utf16_lossy(field_type)
                    )))
                }
            };
            return reference(pool, kind, value, None);
        }
        RefKind::Class => Key::Class(ClassRef::Pool(index(
            value,
            pool.classes.len(),
            "cp_Class",
        )?)),
        RefKind::Signature => Key::Utf8(pool.signature_text(index(
            value,
            pool.signatures.len(),
            "cp_Signature",
        )?)),
        RefKind::Descr => Key::Descr(index(value, pool.descrs.len(), "cp_Descr")?),
        RefKind::Field => Key::Field(index(value, pool.fields.len(), "cp_Field")?),
        RefKind::Method => Key::Method(index(value, pool.methods.len(), "cp_Method")?),
        RefKind::Imethod => Key::Imethod(index(value, pool.imethods.len(), "cp_Imethod")?),
        RefKind::Utf8 => Key::Utf8(Text::Pool(index(value, pool.utf8.len(), "cp_Utf8")?)),
        RefKind::Any => pool.all(value)?,
    })
}
