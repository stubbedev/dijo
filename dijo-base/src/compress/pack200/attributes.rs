//! Attribute definitions (JSR 200 section 5.5): the predefined attributes and those the
//! `attr_definition_bands` define, and the flags and attribute bands of classes, fields,
//! methods and codes (section 5.9), read into the attributes of each.

use std::collections::{BTreeMap, HashMap};

use super::coding::{Bands, BYTE1, UNSIGNED5};
use super::header::{
    Header, HAVE_CLASS_FLAGS_HI, HAVE_CODE_FLAGS_HI, HAVE_FIELD_FLAGS_HI, HAVE_METHOD_FLAGS_HI,
};
use super::layout::{Layout, Piece, Reading};
use super::pool::{index, Pool};
use super::{malformed, Result};

/// The flag bit that marks overflow attributes, counted in an `*_attr_count` band.
const OVERFLOW: u64 = 1 << 16;

/// What attributes apply to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Context {
    Class,
    Field,
    Method,
    Code,
}

const CONTEXTS: [Context; 4] = [
    Context::Class,
    Context::Field,
    Context::Method,
    Context::Code,
];

/// How an attribute is written: as its layout stores it, or by one of the rules for the
/// predefined attributes that the specification gives special processing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Special {
    Plain,
    /// The predefined `SourceFile`, whose null reference stands for the name derived from the
    /// class (section 5.6).
    SourceFile,
    /// The `Code` attribute, read from the `code_bands`.
    Code,
    /// A local `InnerClasses` attribute, an adjustment to the nested classes the class file
    /// is given (section 5.9).
    InnerClasses,
    /// The class-file version pseudo-attribute, stored in the class file's header.
    Version,
}

macro_rules! member_value {
    () => {
        "[TB(66,67,73,83,90)[KIH](68)[KDH](70)[KFH](74)[KJH](99)[RSH](101)[RSHRUH](115)[RUH]\
         (91)[NH[(0)]](64)[RSHNH[RUH(0)]]()[]]"
    };
}

/// The layout of annotations (section 5.5.6).
const ANNOTATIONS: &str = concat!("[NH[(1)]][RSHNH[RUH(1)]]", member_value!());
/// The layout of parameter annotations.
const PARAMETER_ANNOTATIONS: &str = concat!("[NB[(1)]][NH[(1)]][RSHNH[RUH(1)]]", member_value!());
/// The layout of `StackMapTable` (section 5.5.5).
const STACK_MAP_TABLE: &str = "[NH[(1)]][TB(64-127)[(2)](247)[(1)(2)](248-251)[(1)](252)[(1)(2)]\
                               (253)[(1)(2)(2)](254)[(1)(2)(2)(2)](255)[(1)NH[(2)]NH[(2)]]()[]][H]\
                               [TB(7)[RCH](8)[PH]()[]]";
const LOCAL_VARIABLES: &str = "NH[PHOHRUHRSHH]";

/// The predefined attributes (sections 5.5.1 and 5.5.4): their context, index, name and layout.
const PREDEFINED: &[(Context, usize, &str, &str, Special)] = &[
    (
        Context::Class,
        17,
        "SourceFile",
        "RUNH",
        Special::SourceFile,
    ),
    (
        Context::Class,
        18,
        "EnclosingMethod",
        "RCHRDNH",
        Special::Plain,
    ),
    (Context::Class, 19, "Signature", "RSH", Special::Plain),
    (Context::Class, 20, "Deprecated", "", Special::Plain),
    (
        Context::Class,
        21,
        "RuntimeVisibleAnnotations",
        ANNOTATIONS,
        Special::Plain,
    ),
    (
        Context::Class,
        22,
        "RuntimeInvisibleAnnotations",
        ANNOTATIONS,
        Special::Plain,
    ),
    (
        Context::Class,
        23,
        "InnerClasses",
        "NV[RCVTV(0)[]()[RCNVRUNV]]",
        Special::InnerClasses,
    ),
    (
        Context::Class,
        24,
        "class-file version",
        "VV",
        Special::Version,
    ),
    (Context::Field, 17, "ConstantValue", "KQH", Special::Plain),
    (Context::Field, 19, "Signature", "RSH", Special::Plain),
    (Context::Field, 20, "Deprecated", "", Special::Plain),
    (
        Context::Field,
        21,
        "RuntimeVisibleAnnotations",
        ANNOTATIONS,
        Special::Plain,
    ),
    (
        Context::Field,
        22,
        "RuntimeInvisibleAnnotations",
        ANNOTATIONS,
        Special::Plain,
    ),
    (Context::Method, 17, "Code", "", Special::Code),
    (Context::Method, 18, "Exceptions", "NH[RCH]", Special::Plain),
    (Context::Method, 19, "Signature", "RSH", Special::Plain),
    (Context::Method, 20, "Deprecated", "", Special::Plain),
    (
        Context::Method,
        21,
        "RuntimeVisibleAnnotations",
        ANNOTATIONS,
        Special::Plain,
    ),
    (
        Context::Method,
        22,
        "RuntimeInvisibleAnnotations",
        ANNOTATIONS,
        Special::Plain,
    ),
    (
        Context::Method,
        23,
        "RuntimeVisibleParameterAnnotations",
        PARAMETER_ANNOTATIONS,
        Special::Plain,
    ),
    (
        Context::Method,
        24,
        "RuntimeInvisibleParameterAnnotations",
        PARAMETER_ANNOTATIONS,
        Special::Plain,
    ),
    (
        Context::Method,
        25,
        "AnnotationDefault",
        member_value!(),
        Special::Plain,
    ),
    (
        Context::Code,
        0,
        "StackMapTable",
        STACK_MAP_TABLE,
        Special::Plain,
    ),
    (
        Context::Code,
        1,
        "LineNumberTable",
        "NH[PHH]",
        Special::Plain,
    ),
    (
        Context::Code,
        2,
        "LocalVariableTable",
        LOCAL_VARIABLES,
        Special::Plain,
    ),
    (
        Context::Code,
        3,
        "LocalVariableTypeTable",
        LOCAL_VARIABLES,
        Special::Plain,
    ),
];

/// An attribute layout defined for an attribute index of a context.
#[derive(Debug)]
struct Definition {
    name: Vec<u16>,
    layout: Layout,
    special: Special,
    predefined: bool,
}

/// An attribute of a class, field, method or code: its name, how it is written and what its
/// layout read of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Use {
    pub(super) name: Vec<u16>,
    pub(super) special: Special,
    pub(super) pieces: Vec<Piece>,
}

/// A class, field, method or code, as its flags and attribute bands give it: its flags and its
/// attributes, in the order section 7.1 gives them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Entity {
    pub(super) flags: u64,
    pub(super) attributes: Vec<Use>,
}

/// The attribute definitions of a segment, by context and index.
#[derive(Debug)]
pub(super) struct Definitions {
    contexts: [BTreeMap<usize, Definition>; 4],
    have_flags_hi: [bool; 4],
}

impl Definitions {
    /// The predefined attributes and those of the `attr_definition_bands`, which take over
    /// the indexes they are defined for.
    pub(super) fn read(bands: &mut Bands, header: &Header, pool: &Pool) -> Result<Definitions> {
        let have_flags_hi = [
            HAVE_CLASS_FLAGS_HI,
            HAVE_FIELD_FLAGS_HI,
            HAVE_METHOD_FLAGS_HI,
            HAVE_CODE_FLAGS_HI,
        ]
        .map(|option| header.has(option));
        let mut contexts: [BTreeMap<usize, Definition>; 4] = Default::default();
        for &(context, at, name, layout, special) in PREDEFINED {
            let layout: Vec<u16> = layout.encode_utf16().collect();
            contexts[context as usize].insert(
                at,
                Definition {
                    name: name.encode_utf16().collect(),
                    layout: Layout::parse(&layout)?,
                    special,
                    predefined: true,
                },
            );
        }
        let count = header.attr_definition_count;
        let headers = bands.band(BYTE1, count)?;
        let names = bands.band(UNSIGNED5, count)?;
        let layouts = bands.band(UNSIGNED5, count)?;
        let mut overflow = have_flags_hi.map(|hi| if hi { 63 } else { 32 });
        for ((header, name), layout) in headers.into_iter().zip(names).zip(layouts) {
            let context = (header & 3) as usize;
            let bit = (header >> 2) as usize;
            let at = if bit == 0 {
                overflow[context] += 1;
                overflow[context] - 1
            } else {
                bit - 1
            };
            let name = pool.utf8[index(name, pool.utf8.len(), "cp_Utf8")?].clone();
            let layout = &pool.utf8[index(layout, pool.utf8.len(), "cp_Utf8")?];
            contexts[context].insert(
                at,
                Definition {
                    name,
                    layout: Layout::parse(layout)?,
                    special: Special::Plain,
                    predefined: false,
                },
            );
        }
        Ok(Definitions {
            contexts,
            have_flags_hi,
        })
    }

    /// The access flags of `flags`, an entity of `context`'s: its low sixteen bits but those
    /// taken over by attributes.
    pub(super) fn access(&self, context: Context, flags: u64) -> u16 {
        let definitions = &self.contexts[context as usize];
        (0..16)
            .filter(|bit| flags >> bit & 1 == 1 && !definitions.contains_key(bit))
            .fold(0, |access, bit| access | 1 << bit)
    }

    /// The flags and attributes of `count` entities of `context`, read from their flags and
    /// attribute bands, those of fields with their types, `field_types`.
    pub(super) fn read_context(
        &self,
        bands: &mut Bands,
        pool: &Pool,
        context: Context,
        count: usize,
        field_types: &[&[u16]],
    ) -> Result<Vec<Entity>> {
        let at = CONTEXTS
            .iter()
            .position(|&each| each == context)
            .expect("a context");
        let definitions = &self.contexts[at];
        let high = if self.have_flags_hi[at] {
            Some(bands.band(UNSIGNED5, count)?)
        } else {
            None
        };
        let low = bands.band(UNSIGNED5, count)?;
        let flags: Vec<u64> = low
            .iter()
            .enumerate()
            .map(|(entity, &low)| {
                let high = high.as_ref().map_or(0, |high| high[entity]);
                u64::from(high as u32) << 32 | u64::from(low as u32)
            })
            .collect();
        let overflowed = flags.iter().filter(|&&flags| flags & OVERFLOW != 0).count();
        let attr_counts = bands.counts(UNSIGNED5, overflowed)?;
        let total = attr_counts
            .iter()
            .try_fold(0usize, |sum, &count| sum.checked_add(count))
            .ok_or_else(|| malformed("the overflow attribute counts overflow"))?;
        let indexes = bands.counts(UNSIGNED5, total)?;
        let (mut attr_counts, mut indexes) = (attr_counts.into_iter(), indexes.into_iter());
        let mut lists = Vec::with_capacity(flags.len());
        let mut occurrences: HashMap<usize, usize> = HashMap::new();
        for &entity_flags in &flags {
            let mut list: Vec<usize> = (0..63)
                .filter(|&bit| {
                    bit != 16 && entity_flags >> bit & 1 == 1 && definitions.contains_key(&bit)
                })
                .collect();
            if entity_flags & OVERFLOW != 0 {
                let overflow = attr_counts.next().expect("a count per overflowing entity");
                for _ in 0..overflow {
                    let index = indexes.next().expect("an index per counted attribute");
                    if !definitions.contains_key(&index) {
                        return Err(malformed(format!(
                            "an overflow attribute of the undefined index {index}"
                        )));
                    }
                    list.push(index);
                }
            }
            for &index in &list {
                *occurrences.entry(index).or_default() += 1;
            }
            lists.push(list);
        }
        let order: Vec<usize> = definitions
            .iter()
            .filter(|(_, definition)| definition.predefined)
            .chain(
                definitions
                    .iter()
                    .filter(|(_, definition)| !definition.predefined),
            )
            .map(|(&index, _)| index)
            .filter(|index| occurrences.contains_key(index))
            .collect();
        let call_count = order
            .iter()
            .map(|index| definitions[index].layout.backward_callables())
            .sum();
        let mut calls = bands.counts(UNSIGNED5, call_count)?.into_iter();
        let mut readings: HashMap<usize, Reading> = HashMap::new();
        for index in order {
            let definition = &definitions[&index];
            if definition.special != Special::Code {
                let reading = definition
                    .layout
                    .read(bands, occurrences[&index], &mut calls)?;
                readings.insert(index, reading);
            }
        }
        let mut entities = Vec::with_capacity(flags.len());
        for (entity, (flags, list)) in flags.into_iter().zip(lists).enumerate() {
            let mut attributes = Vec::with_capacity(list.len());
            for index in list {
                let definition = &definitions[&index];
                let pieces = match readings.get_mut(&index) {
                    Some(reading) => reading.next(pool, field_types.get(entity).copied())?,
                    None => Vec::new(),
                };
                attributes.push(Use {
                    name: definition.name.clone(),
                    special: definition.special,
                    pieces,
                });
            }
            entities.push(Entity { flags, attributes });
        }
        Ok(entities)
    }
}
