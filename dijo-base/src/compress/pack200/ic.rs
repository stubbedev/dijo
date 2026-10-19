//! The inner class bands (JSR 200 section 5.7): `ic_All`, the nested class records of a
//! segment, read as commons-compress's `IcBands.read` reads them.

use super::coding::{Bands, DELTA5, UDELTA5, UNSIGNED5};
use super::header::Header;
use super::pool::{index, ClassRef, Pool, Text};
use super::Result;

/// The flag of a nested class whose outer class and name are transmitted.
const EXPLICIT: i32 = 1 << 16;

/// A nested class record `<C,F,C2,N>`: the class, its flags, its outer class and its simple
/// name, by their spellings.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct Tuple {
    /// The bytecode name of the nested class.
    pub(super) this: Vec<u16>,
    /// The flags as the class file stores them, without the mark of transmitted names.
    pub(super) flags: u16,
    /// The bytecode name of the outer class, transmitted or predicted, none for none.
    pub(super) outer: Option<Vec<u16>>,
    /// The simple name, transmitted or predicted, none for an anonymous class.
    pub(super) name: Option<Vec<u16>>,
}

/// `ic_All`, read from the `ic_bands`: `ic_this_class`, `ic_flags`, then for the tuples
/// whose flags have bit 16 set `ic_outer_class` and `ic_name`, the outer class and name of
/// the others predicted from the name of the class ([`predict`]).
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

/// The class constant and string constant of a tuple's class, outer class and name, which
/// the `InnerClasses` attribute of a rebuilt class file refers to.
pub(super) fn tuple_keys(tuple: &Tuple, pool: &Pool) -> (ClassRef, Option<ClassRef>, Option<Text>) {
    (
        pool.class_named(&tuple.this),
        tuple.outer.as_deref().map(|outer| pool.class_named(outer)),
        tuple.name.as_deref().map(|name| pool.text(name)),
    )
}
