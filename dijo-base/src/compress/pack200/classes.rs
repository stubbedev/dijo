//! Nested classes (JSR 200 section 5.7) and the bands of classes, fields, methods and codes
//! (sections 5.8 and 5.9), read into the classes of a segment.

use super::attributes::{Context, Definitions, Entity, Special, Use};
use super::bytecode::Bytecode;
use super::coding::{Bands, BCI5, BRANCH5, BYTE1, DELTA5, MDELTA5, UDELTA5, UNSIGNED5};
use super::header::{Header, HAVE_ALL_CODE_FLAGS};
use super::layout::Piece;
use super::pool::{index, ClassRef, Key, Pool, Text};
use super::{malformed, Result};

/// The flag of a nested class whose outer class and name are transmitted.
const EXPLICIT: i32 = 1 << 16;

/// A nested class record `<C,F,C2,N>`: the class, its flags, its outer class and its simple
/// name, by their spellings.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Tuple {
    pub(super) this: Vec<u16>,
    /// The flags as the class file stores them, without the mark of transmitted names.
    pub(super) flags: u16,
    pub(super) outer: Option<Vec<u16>>,
    pub(super) name: Option<Vec<u16>>,
}

/// A class of a segment.
#[derive(Debug)]
pub(super) struct Class {
    pub(super) this: usize,
    /// The super class, none for `java/lang/Object`, whose super class is transmitted as
    /// itself.
    pub(super) super_class: Option<usize>,
    pub(super) interfaces: Vec<usize>,
    pub(super) flags: u64,
    /// Its attributes, but the local `InnerClasses` and the class-file version.
    pub(super) attributes: Vec<Use>,
    /// The minor and major version of its class file.
    pub(super) version: (u16, u16),
    /// `ic_Local`, if transmitted.
    pub(super) local_tuples: Option<Vec<Tuple>>,
    pub(super) fields: Vec<Member>,
    pub(super) methods: Vec<Member>,
}

/// A field or method.
#[derive(Debug)]
pub(super) struct Member {
    pub(super) descr: usize,
    pub(super) flags: u64,
    pub(super) attributes: Vec<Use>,
    /// The `Code` attribute of a method that has one.
    pub(super) code: Option<Code>,
}

/// A `Code` attribute.
#[derive(Debug)]
pub(super) struct Code {
    pub(super) max_stack: usize,
    pub(super) max_na_locals: usize,
    pub(super) handlers: Vec<Handler>,
    pub(super) attributes: Vec<Use>,
    /// The bytecodes, once the `bc_bands` are read.
    pub(super) bytecode: Option<Bytecode>,
}

/// An exception handler, its bytecode indexes renumbered.
#[derive(Debug)]
pub(super) struct Handler {
    pub(super) start: i32,
    pub(super) end: i32,
    pub(super) catch: i32,
    pub(super) class: Option<usize>,
}

/// `ic_All`, read from the `ic_bands`.
pub(super) fn read_tuples(bands: &mut Bands, header: &Header, pool: &Pool) -> Result<Vec<Tuple>> {
    let count = header.ic_count;
    let classes = bands.band(UDELTA5, count)?;
    let flags = bands.band(UNSIGNED5, count)?;
    let explicit = flags.iter().filter(|&&flags| flags & EXPLICIT != 0).count();
    let outers = bands.band(DELTA5, explicit)?;
    let names = bands.band(DELTA5, explicit)?;
    let (mut outers, mut names) = (outers.into_iter(), names.into_iter());
    let mut tuples = Vec::with_capacity(count);
    for (class, flags) in classes.into_iter().zip(flags) {
        let this = pool.utf8[pool.classes[index(class, pool.classes.len(), "cp_Class")?]].clone();
        let (outer, name) = if flags & EXPLICIT != 0 {
            let outer = outers.next().expect("an outer class per explicit tuple");
            let name = names.next().expect("a name per explicit tuple");
            let outer = match outer {
                0 => None,
                outer => {
                    let class = index(outer - 1, pool.classes.len(), "cp_Class")?;
                    Some(pool.utf8[pool.classes[class]].clone())
                }
            };
            let name = match name {
                0 => None,
                name => Some(pool.utf8[index(name - 1, pool.utf8.len(), "cp_Utf8")?].clone()),
            };
            (outer, name)
        } else {
            predict(&this)
        };
        tuples.push(Tuple {
            this,
            flags: flags as u16,
            outer,
            name,
        });
    }
    Ok(tuples)
}

/// The outer class and simple name predictable from the bytecode name of a nested class, by
/// the grammar of section 5.7, whose DOLLAR is any character up to `-`, SLASH `/` or `.`,
/// DIGIT an ASCII digit and LETTER any other character.
pub(super) fn predict(name: &[u16]) -> (Option<Vec<u16>>, Option<Vec<u16>>) {
    let dollar = |c: &u16| *c <= 0x2D;
    let digit = |c: &u16| (0x30..=0x39).contains(c);
    let letter = |c: &u16| *c >= 0x3A;
    let start = name
        .iter()
        .rposition(|&c| c == 0x2E || c == 0x2F)
        .map_or(0, |slash| slash + 1);
    let simple = &name[start..];
    let Some(last) = simple.iter().rposition(dollar) else {
        return (None, None);
    };
    let (head, tail) = (&simple[..last], &simple[last + 1..]);
    let is_number = |part: &[u16]| !part.is_empty() && part.iter().all(digit);
    if is_number(tail) {
        return (None, None);
    }
    let predictable_name =
        tail.first().is_some_and(letter) && tail.iter().all(|c| letter(c) || digit(c));
    if !predictable_name {
        return (None, None);
    }
    if let Some(before) = head.iter().rposition(dollar) {
        if is_number(&head[before + 1..]) {
            return (None, Some(tail.to_vec()));
        }
    }
    if head.is_empty() {
        return (None, None);
    }
    (Some(name[..start + last].to_vec()), Some(tail.to_vec()))
}

/// The tuples of a local `InnerClasses` attribute, as its layout read them: their count, then
/// for each its class and flags, and where the flags are not zero its outer class and name,
/// else those of the tuple of `ic_All` for the class.
fn local_tuples(pieces: &[Piece], pool: &Pool, tuples: &[Tuple]) -> Result<Vec<Tuple>> {
    let mut local = Vec::new();
    let mut pieces = pieces.iter().skip(1);
    let name_of = |piece: Option<&Piece>| -> Result<Option<Vec<u16>>> {
        Ok(match piece {
            Some(Piece::Ref {
                key: Some(Key::Class(class)),
                ..
            }) => Some(pool.class_name(class).to_vec()),
            Some(Piece::Ref {
                key: Some(Key::Utf8(text)),
                ..
            }) => Some(pool.spelling(text).to_vec()),
            Some(Piece::Ref { key: None, .. }) => None,
            _ => return Err(malformed("a local InnerClasses attribute is malformed")),
        })
    };
    while let Some(class) = pieces.next() {
        let this = name_of(Some(class))?
            .ok_or_else(|| malformed("a local InnerClasses tuple of no class"))?;
        let Some(Piece::Int { value: flags, .. }) = pieces.next() else {
            return Err(malformed("a local InnerClasses tuple without flags"));
        };
        if *flags == 0 {
            let global = tuples
                .iter()
                .find(|tuple| tuple.this == this)
                .ok_or_else(|| malformed("a local InnerClasses tuple of no nested class"))?;
            local.push(global.clone());
        } else {
            let outer = name_of(pieces.next())?;
            let name = name_of(pieces.next())?;
            local.push(Tuple {
                this,
                flags: *flags as u16,
                outer,
                name,
            });
        }
    }
    Ok(local)
}

/// The sum of `counts`, checked.
fn sum(counts: &[usize]) -> Result<usize> {
    counts
        .iter()
        .try_fold(0usize, |sum, &count| sum.checked_add(count))
        .ok_or_else(|| malformed("the counts of a band overflow"))
}

fn references(values: Vec<i32>, len: usize, pool: &str) -> Result<Vec<usize>> {
    values
        .into_iter()
        .map(|value| index(value, len, pool))
        .collect()
}

/// The classes of a segment, read from the `class_bands`, `ic_All` being `tuples`.
pub(super) fn read_classes(
    bands: &mut Bands,
    header: &Header,
    pool: &Pool,
    definitions: &Definitions,
    tuples: &[Tuple],
) -> Result<Vec<Class>> {
    let count = header.class_count;
    let class_count = pool.classes.len();
    let this = references(bands.band(DELTA5, count)?, class_count, "cp_Class")?;
    let supers = references(bands.band(DELTA5, count)?, class_count, "cp_Class")?;
    let interface_counts = bands.counts(DELTA5, count)?;
    let interfaces = references(
        bands.band(DELTA5, sum(&interface_counts)?)?,
        class_count,
        "cp_Class",
    )?;
    let field_counts = bands.counts(DELTA5, count)?;
    let method_counts = bands.counts(DELTA5, count)?;
    let field_descrs = references(
        bands.band(DELTA5, sum(&field_counts)?)?,
        pool.descrs.len(),
        "cp_Descr",
    )?;
    let field_types: Vec<&[u16]> = field_descrs
        .iter()
        .map(|&descr| pool.descr(descr).1)
        .collect();
    let fields =
        definitions.read_context(bands, pool, Context::Field, field_descrs.len(), &field_types)?;
    let method_descrs = references(
        bands.band(MDELTA5, sum(&method_counts)?)?,
        pool.descrs.len(),
        "cp_Descr",
    )?;
    let methods = definitions.read_context(bands, pool, Context::Method, method_descrs.len(), &[])?;
    let class_entities = definitions.read_context(bands, pool, Context::Class, count, &[])?;
    let mut codes = read_codes(bands, header, pool, definitions, &methods)?.into_iter();

    let mut fields = field_descrs.into_iter().zip(fields);
    let mut methods = method_descrs.into_iter().zip(methods);
    let mut interfaces = interfaces.into_iter();
    let mut classes = Vec::with_capacity(count);
    for (at, entity) in class_entities.into_iter().enumerate() {
        let member = |(descr, entity): (usize, Entity)| Member {
            descr,
            flags: entity.flags,
            attributes: entity.attributes,
            code: None,
        };
        let class_fields = fields.by_ref().take(field_counts[at]).map(member).collect();
        let mut class_methods: Vec<Member> = methods
            .by_ref()
            .take(method_counts[at])
            .map(member)
            .collect();
        for method in &mut class_methods {
            if method
                .attributes
                .iter()
                .any(|attribute| attribute.special == Special::Code)
            {
                method.code = Some(codes.next().expect("a code per method with one"));
            }
        }
        let mut version = (header.default_minor, header.default_major);
        let mut local: Option<Vec<Tuple>> = None;
        let mut attributes = Vec::with_capacity(entity.attributes.len());
        for attribute in entity.attributes {
            match attribute.special {
                Special::Version => {
                    let mut values = attribute.pieces.iter().map(|piece| match piece {
                        Piece::Int { value, .. } => *value as u16,
                        _ => 0,
                    });
                    version = (values.next().unwrap_or(0), values.next().unwrap_or(0));
                }
                Special::InnerClasses => {
                    local
                        .get_or_insert_with(Vec::new)
                        .extend(local_tuples(&attribute.pieces, pool, tuples)?);
                }
                _ => attributes.push(attribute),
            }
        }
        classes.push(Class {
            this: this[at],
            super_class: Some(supers[at]).filter(|&super_class| super_class != this[at]),
            interfaces: interfaces.by_ref().take(interface_counts[at]).collect(),
            flags: entity.flags,
            attributes,
            version,
            local_tuples: local,
            fields: class_fields,
            methods: class_methods,
        });
    }
    Ok(classes)
}

/// The `Code` attributes of the methods `methods`, read from the `code_bands`.
fn read_codes(
    bands: &mut Bands,
    header: &Header,
    pool: &Pool,
    definitions: &Definitions,
    methods: &[Entity],
) -> Result<Vec<Code>> {
    let count = methods
        .iter()
        .filter(|method| {
            method
                .attributes
                .iter()
                .any(|attribute| attribute.special == Special::Code)
        })
        .count();
    let headers = bands.band(BYTE1, count)?;
    let special = headers.iter().filter(|&&header| header == 0).count();
    let max_stacks = bands.counts(UNSIGNED5, special)?;
    let max_na_locals = bands.counts(UNSIGNED5, special)?;
    let handler_counts = bands.counts(UNSIGNED5, special)?;
    let (mut max_stacks, mut max_na_locals, mut handler_counts) = (
        max_stacks.into_iter(),
        max_na_locals.into_iter(),
        handler_counts.into_iter(),
    );
    let mut shapes = Vec::with_capacity(count);
    for &code_header in &headers {
        let header = code_header as usize;
        shapes.push(match header {
            0 => (
                max_stacks.next().expect("a stack size per special header"),
                max_na_locals.next().expect("a locals size per special header"),
                handler_counts.next().expect("a handler count per special header"),
            ),
            1..=144 => ((header - 1) % 12, (header - 1) / 12, 0),
            145..=208 => ((header - 145) % 8, (header - 145) / 8, 1),
            _ => ((header - 209) % 7, (header - 209) / 7, 2),
        });
    }
    let handler_total = shapes
        .iter()
        .try_fold(0usize, |sum, &(_, _, handlers)| sum.checked_add(handlers))
        .ok_or_else(|| malformed("the handler counts overflow"))?;
    let starts = bands.band(BCI5, handler_total)?;
    let ends = bands.band(BRANCH5, handler_total)?;
    let catches = bands.band(BRANCH5, handler_total)?;
    let handler_classes = bands.band(UNSIGNED5, handler_total)?;
    let mut handlers = Vec::with_capacity(handler_total);
    for (((start, end), catch), class) in starts.into_iter().zip(ends).zip(catches).zip(handler_classes) {
        let end = start.wrapping_add(end);
        handlers.push(Handler {
            start,
            end,
            catch: end.wrapping_add(catch),
            class: match class {
                0 => None,
                class => Some(index(class - 1, pool.classes.len(), "cp_Class")?),
            },
        });
    }
    let all_flags = header.has(HAVE_ALL_CODE_FLAGS);
    let flagged = headers
        .iter()
        .filter(|&&header| all_flags || header == 0)
        .count();
    let mut entities = definitions
        .read_context(bands, pool, Context::Code, flagged, &[])?
        .into_iter();
    let mut handlers = handlers.into_iter();
    let mut codes = Vec::with_capacity(count);
    for (&code_header, (max_stack, max_na_locals, handler_count)) in headers.iter().zip(shapes) {
        let attributes = if all_flags || code_header == 0 {
            entities
                .next()
                .expect("an entity per flagged code")
                .attributes
        } else {
            Vec::new()
        };
        codes.push(Code {
            max_stack,
            max_na_locals,
            handlers: handlers.by_ref().take(handler_count).collect(),
            attributes,
            bytecode: None,
        });
    }
    Ok(codes)
}

/// The class constant and string constant of a tuple's class, outer class and name.
pub(super) fn tuple_keys(tuple: &Tuple, pool: &Pool) -> (ClassRef, Option<ClassRef>, Option<Text>) {
    (
        pool.class_named(&tuple.this),
        tuple.outer.as_deref().map(|outer| pool.class_named(outer)),
        tuple.name.as_deref().map(|name| pool.text(name)),
    )
}
