//! Class file reconstruction (JSR 200 section 7): the class file of a class of a segment,
//! rebuilt from its bands as commons-compress's `Segment.createClassFile` and the JDK
//! unpacker rebuild it.
//!
//! A class file is written twice. The first pass only collects the constants it refers to,
//! from which the relevant nested classes and the `InnerClasses` attribute follow
//! (section 7.2); the constant pool is then ordered as section 7.2 orders it, and the second
//! pass writes the class file with the indexes of that pool.

use std::collections::{BTreeSet, HashMap, HashSet};

use super::attributes::{Context, Definitions, Special, Use};
use super::bytecode::{Instruction, Operand};
use super::classes::{Class, Code, Member};
use super::ic::{tuple_keys, Tuple};
use super::layout::{IntKind, Piece};
use super::pool::{ClassRef, Key, Pool, Text};
use super::{malformed, Result};

/// The access flag of a static method, which takes no `this` among its locals.
const ACC_STATIC: u16 = 0x0008;

/// The opcode of `aload_0`, which the `aload_0_*` forms put before their instruction.
const ALOAD_0: u8 = 42;

/// The opcode of `wide`.
const WIDE: u8 = 196;

/// The class file of `class`, a class of a segment of the constants `pool`, the attribute
/// definitions `definitions` and the nested class records `tuples` (`ic_All`).
pub(super) fn class_file(
    pool: &Pool,
    definitions: &Definitions,
    tuples: &[Tuple],
    class: &Class,
) -> Result<Vec<u8>> {
    let builder = Builder {
        pool,
        definitions,
        class,
    };
    let mut collected = Out::new(None);
    builder.body(&mut collected, None)?;
    close(pool, &mut collected.keys);
    let stored = stored_tuples(pool, tuples, class, &collected.keys);
    if let Some(stored) = &stored {
        builder.inner_classes(&mut collected, stored)?;
        close(pool, &mut collected.keys);
    }
    let constants = Constants::new(pool, collected.keys, &collected.narrow)?;
    let mut out = Out::new(Some(&constants));
    out.u4(0xCAFE_BABE);
    out.u2(class.version.0);
    out.u2(class.version.1);
    constants.write(pool, &mut out)?;
    builder.body(&mut out, stored.as_deref())?;
    Ok(out.bytes)
}

/// Adds to `keys` every constant the constants of `keys` refer to, until none is missing.
fn close(pool: &Pool, keys: &mut HashSet<Key>) {
    let mut pending: Vec<Key> = keys.iter().cloned().collect();
    while let Some(key) = pending.pop() {
        for referred in pool.referred(&key) {
            if keys.insert(referred.clone()) {
                pending.push(referred);
            }
        }
    }
}

/// `ic_Stored` of `class`, the tuples of its `InnerClasses` attribute, none for no attribute
/// (section 7.2): its local tuples and its relevant tuples, less those in both.
///
/// The relevant tuples are those of `ic_All` whose outer class is the class, and those of the
/// classes `keys` names, closed over the outer classes of the tuples added, as the JDK
/// unpacker closes them, in the order of `ic_All`.
fn stored_tuples(
    pool: &Pool,
    tuples: &[Tuple],
    class: &Class,
    keys: &HashSet<Key>,
) -> Option<Vec<Tuple>> {
    let this = pool.class_name(&ClassRef::Pool(class.this));
    let mut by_name: HashMap<&[u16], usize> = HashMap::new();
    for (at, tuple) in tuples.iter().enumerate() {
        by_name.entry(tuple.this.as_slice()).or_insert(at);
    }
    let mut relevant: BTreeSet<usize> = tuples
        .iter()
        .enumerate()
        .filter(|(_, tuple)| tuple.outer.as_deref() == Some(this))
        .map(|(at, _)| at)
        .collect();
    let mut names: Vec<&[u16]> = keys
        .iter()
        .filter_map(|key| match key {
            Key::Class(class) => Some(pool.class_name(class)),
            _ => None,
        })
        .collect();
    while let Some(name) = names.pop() {
        if let Some(&at) = by_name.get(name) {
            if relevant.insert(at) {
                if let Some(outer) = &tuples[at].outer {
                    names.push(outer.as_slice());
                }
            }
        }
    }
    let relevant: Vec<&Tuple> = relevant.into_iter().map(|at| &tuples[at]).collect();
    let local = match &class.local_tuples {
        Some(local) if local.is_empty() => return None,
        None if relevant.is_empty() => return None,
        local => local.as_deref().unwrap_or_default(),
    };
    let mut stored: Vec<Tuple> = local
        .iter()
        .filter(|tuple| !relevant.contains(tuple))
        .cloned()
        .collect();
    stored.extend(
        relevant
            .into_iter()
            .filter(|tuple| !local.contains(*tuple))
            .cloned(),
    );
    Some(stored)
}

/// The constant pool of a class file: its constants in order, and the index of each.
struct Constants {
    order: Vec<Key>,
    indexes: HashMap<Key, u16>,
    /// `constant_pool_count`, one more than the last index taken.
    count: u16,
}

impl Constants {
    /// The constants `keys` in the order of section 7.2: those transmitted in the order of
    /// `cp_All`, then the strings and the classes not transmitted, each by its spelling, and
    /// those of `narrow`, referred to by a one-byte index, moved to the front in that order.
    fn new(pool: &Pool, keys: HashSet<Key>, narrow: &HashSet<Key>) -> Result<Constants> {
        let mut order: Vec<Key> = keys.into_iter().collect();
        order.sort_by_cached_key(|key| pool.rank(key));
        let (mut front, back): (Vec<Key>, Vec<Key>) =
            order.into_iter().partition(|key| narrow.contains(key));
        front.extend(back);
        let mut indexes = HashMap::with_capacity(front.len());
        let mut next = 1u32;
        for key in &front {
            let index = u16::try_from(next)
                .map_err(|_| malformed("a class file of more than 65535 constants"))?;
            indexes.insert(key.clone(), index);
            next += match key {
                Key::Long(_) | Key::Double(_) => 2,
                _ => 1,
            };
        }
        let count = u16::try_from(next)
            .map_err(|_| malformed("a class file of more than 65535 constants"))?;
        Ok(Constants {
            order: front,
            indexes,
            count,
        })
    }

    /// The index of `key`.
    fn index(&self, key: &Key) -> Result<u16> {
        self.indexes
            .get(key)
            .copied()
            .ok_or_else(|| malformed("a constant missing from the rebuilt constant pool"))
    }

    /// Writes `constant_pool_count` and the constants to `out`.
    fn write(&self, pool: &Pool, out: &mut Out) -> Result<()> {
        out.u2(self.count);
        for key in &self.order {
            match key {
                Key::Utf8(text) => {
                    out.u1(1);
                    let bytes = modified_utf8(pool.spelling(text));
                    out.count(bytes.len(), "bytes of a CONSTANT_Utf8")?;
                    out.bytes.extend_from_slice(&bytes);
                }
                Key::Int(index) => {
                    out.u1(3);
                    out.u4(pool.ints[*index] as u32);
                }
                Key::Float(index) => {
                    out.u1(4);
                    out.u4(pool.floats[*index]);
                }
                Key::Long(index) => {
                    out.u1(5);
                    out.bytes.extend_from_slice(&pool.longs[*index].to_be_bytes());
                }
                Key::Double(index) => {
                    out.u1(6);
                    out.bytes.extend_from_slice(&pool.doubles[*index].to_be_bytes());
                }
                Key::Class(_) => out.u1(7),
                Key::String(_) => out.u1(8),
                Key::Field(_) => out.u1(9),
                Key::Method(_) => out.u1(10),
                Key::Imethod(_) => out.u1(11),
                Key::Descr(_) => out.u1(12),
            }
            for referred in pool.referred(key) {
                out.reference(Some(&referred), 2)?;
            }
        }
        Ok(())
    }
}

/// `units` in the modified UTF-8 of class files: NUL and the code units past U+007F in two
/// or three bytes, each surrogate on its own.
fn modified_utf8(units: &[u16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(units.len());
    for &unit in units {
        match unit {
            0x0001..=0x007F => bytes.push(unit as u8),
            0x0000 | 0x0080..=0x07FF => {
                bytes.push(0xC0 | (unit >> 6) as u8);
                bytes.push(0x80 | (unit & 0x3F) as u8);
            }
            _ => {
                bytes.push(0xE0 | (unit >> 12) as u8);
                bytes.push(0x80 | (unit >> 6 & 0x3F) as u8);
                bytes.push(0x80 | (unit & 0x3F) as u8);
            }
        }
    }
    bytes
}

/// Where a class file is written: its bytes, and while the constants are collected rather
/// than indexed, the constants it refers to.
struct Out<'c> {
    bytes: Vec<u8>,
    /// The constant pool, none while the constants are collected.
    constants: Option<&'c Constants>,
    /// The constants referred to.
    keys: HashSet<Key>,
    /// The constants referred to by a one-byte index.
    narrow: HashSet<Key>,
}

impl<'c> Out<'c> {
    fn new(constants: Option<&'c Constants>) -> Self {
        Out {
            bytes: Vec::new(),
            constants,
            keys: HashSet::new(),
            narrow: HashSet::new(),
        }
    }

    fn u1(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u2(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    fn u4(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    /// Writes `value` in `size` bytes, none for a 'V' element, failing where it does not fit
    /// them as a signed or an unsigned number.
    fn sized(&mut self, value: i64, size: u8) -> Result<()> {
        if size == 0 {
            return Ok(());
        }
        if size > 4 {
            return Err(malformed(format!("a value of {size} bytes")));
        }
        let bits = 8 * u32::from(size);
        if value < -(1i64 << (bits - 1)) || value >= 1i64 << bits {
            return Err(malformed(format!(
                "the value {value} does not fit in {size} bytes"
            )));
        }
        self.bytes
            .extend_from_slice(&value.to_be_bytes()[8 - usize::from(size)..]);
        Ok(())
    }

    /// Writes the count `count` of `what` in two bytes.
    fn count(&mut self, count: usize, what: &str) -> Result<()> {
        let count = u16::try_from(count)
            .map_err(|_| malformed(format!("a class file of {count} {what}")))?;
        self.u2(count);
        Ok(())
    }

    /// Writes the index of `key` in `size` bytes, zero for none.
    fn reference(&mut self, key: Option<&Key>, size: u8) -> Result<()> {
        let index = match (key, self.constants) {
            (None, _) => 0,
            (Some(key), Some(constants)) => constants.index(key)?,
            (Some(key), None) => {
                self.keys.insert(key.clone());
                if size == 1 {
                    self.narrow.insert(key.clone());
                }
                0
            }
        };
        if size == 0 {
            return Err(malformed("a constant reference of no bytes"));
        }
        self.sized(i64::from(index), size)
            .map_err(|_| malformed(format!("the constant {index} referred to in {size} bytes")))
    }

    /// Writes the index of the string constant `text` in two bytes.
    fn utf8(&mut self, text: Text) -> Result<()> {
        self.reference(Some(&Key::Utf8(text)), 2)
    }

    /// Writes the index of the class constant `class` in two bytes.
    fn class(&mut self, class: ClassRef) -> Result<()> {
        self.reference(Some(&Key::Class(class)), 2)
    }

    /// Writes an attribute named `name` whose body `body` writes, its length before it.
    fn attribute(&mut self, name: Text, body: impl FnOnce(&mut Self) -> Result<()>) -> Result<()> {
        self.utf8(name)?;
        let at = self.bytes.len();
        self.u4(0);
        body(self)?;
        let length = u32::try_from(self.bytes.len() - at - 4)
            .map_err(|_| malformed("an attribute of more than 4 GiB"))?;
        self.bytes[at..at + 4].copy_from_slice(&length.to_be_bytes());
        Ok(())
    }
}

/// What a class file is rebuilt from.
struct Builder<'s> {
    pool: &'s Pool,
    definitions: &'s Definitions,
    class: &'s Class,
}

impl Builder<'_> {
    /// Writes the class file after its constant pool, with an `InnerClasses` attribute of
    /// `stored` last where it has one.
    fn body(&self, out: &mut Out, stored: Option<&[Tuple]>) -> Result<()> {
        let class = self.class;
        out.u2(self.definitions.access(Context::Class, class.flags));
        out.class(ClassRef::Pool(class.this))?;
        match class.super_class {
            Some(super_class) => out.class(ClassRef::Pool(super_class))?,
            None => out.u2(0),
        }
        out.count(class.interfaces.len(), "interfaces")?;
        for &interface in &class.interfaces {
            out.class(ClassRef::Pool(interface))?;
        }
        out.count(class.fields.len(), "fields")?;
        for field in &class.fields {
            self.member(out, field, Context::Field)?;
        }
        out.count(class.methods.len(), "methods")?;
        for method in &class.methods {
            self.member(out, method, Context::Method)?;
        }
        out.count(
            class.attributes.len() + usize::from(stored.is_some()),
            "class attributes",
        )?;
        for attribute in &class.attributes {
            self.attribute(out, attribute, None)?;
        }
        if let Some(stored) = stored {
            self.inner_classes(out, stored)?;
        }
        Ok(())
    }

    /// Writes the field or method `member`, of `context`.
    fn member(&self, out: &mut Out, member: &Member, context: Context) -> Result<()> {
        let (name, signature) = self.pool.descrs[member.descr];
        out.u2(self.definitions.access(context, member.flags));
        out.utf8(Text::Pool(name))?;
        out.utf8(self.pool.signature_text(signature))?;
        out.count(member.attributes.len(), "attributes of a member")?;
        for attribute in &member.attributes {
            if attribute.special == Special::Code {
                let code = member
                    .code
                    .as_ref()
                    .ok_or_else(|| malformed("a Code attribute without its code bands"))?;
                self.code(out, attribute, code, member)?;
            } else {
                self.attribute(out, attribute, None)?;
            }
        }
        Ok(())
    }

    /// Writes the attribute `attribute` as its layout stores it, its bytecode indexes those
    /// of the code `bcis` renumbers, if it is an attribute of a code.
    fn attribute(&self, out: &mut Out, attribute: &Use, bcis: Option<&Bcis>) -> Result<()> {
        let name = self.pool.text(&attribute.name);
        match attribute.special {
            Special::SourceFile
                if matches!(attribute.pieces.as_slice(), [Piece::Ref { key: None, .. }]) =>
            {
                let file = self.pool.text(&self.source_file());
                out.attribute(name, |out| out.utf8(file))
            }
            Special::Plain | Special::SourceFile => {
                out.attribute(name, |out| pieces(out, &attribute.pieces, bcis))
            }
            Special::Code | Special::InnerClasses | Special::Version => Err(malformed(format!(
                "a {} attribute where it cannot be written",
                String::from_utf16_lossy(&attribute.name)
            ))),
        }
    }

    /// The name of the source file a null `SourceFile` stands for: the simple name of the
    /// class up to its first character no greater than `$`, followed by `.java`, as
    /// commons-compress's `ClassBands` derives it.
    fn source_file(&self) -> Vec<u16> {
        let name = self.pool.class_name(&ClassRef::Pool(self.class.this));
        let start = name
            .iter()
            .rposition(|&c| c == u16::from(b'/') || c == u16::from(b'.'))
            .map_or(0, |at| at + 1);
        let simple = &name[start..];
        let end = simple
            .iter()
            .position(|&c| c <= u16::from(b'$'))
            .unwrap_or(simple.len());
        let mut file = simple[..end].to_vec();
        file.extend(".java".encode_utf16());
        file
    }

    /// Writes the `InnerClasses` attribute of the tuples `stored`.
    fn inner_classes(&self, out: &mut Out, stored: &[Tuple]) -> Result<()> {
        let name: Vec<u16> = "InnerClasses".encode_utf16().collect();
        out.attribute(self.pool.text(&name), |out| {
            out.count(stored.len(), "nested classes")?;
            for tuple in stored {
                let (this, outer, name) = tuple_keys(tuple, self.pool);
                out.class(this)?;
                out.reference(outer.map(Key::Class).as_ref(), 2)?;
                out.reference(name.map(Key::Utf8).as_ref(), 2)?;
                out.u2(tuple.flags);
            }
            Ok(())
        })
    }
}

/// Writes the values `pieces` of an attribute, its bytecode indexes those of the code `bcis`
/// renumbers (section 5.5.3): a 'P' element transmits a renumbered index, a 'PO' element the
/// difference of its renumbered index from that of the element before it, and an 'O' element
/// the difference of the renumbered index it reaches from that of the element before it.
fn pieces(out: &mut Out, pieces: &[Piece], bcis: Option<&Bcis>) -> Result<()> {
    let mut previous = 0i32;
    for piece in pieces {
        match piece {
            Piece::Int { size, value } => out.sized(i64::from(*value), *size)?,
            Piece::Ref { size, key } => out.reference(key.as_ref(), *size)?,
            Piece::Bci { size, kind, value } => {
                let bcis = bcis.ok_or_else(|| {
                    malformed("a bytecode index in an attribute outside of a code")
                })?;
                let stored = match kind {
                    IntKind::Plain => i64::from(*value),
                    IntKind::Bci => {
                        previous = *value;
                        bcis.decode(previous)
                    }
                    IntKind::BciAfter => {
                        previous = previous.wrapping_add(*value);
                        bcis.decode(previous)
                    }
                    IntKind::Offset => {
                        bcis.decode(previous.wrapping_add(*value)) - bcis.decode(previous)
                    }
                };
                out.sized(stored, *size)?;
            }
        }
    }
    Ok(())
}
