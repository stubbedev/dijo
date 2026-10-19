//! The constant pools of a segment (JSR 200 section 5.3), read from the `cp_bands`, and the
//! constants class files refer to, named by their place in `cp_All` or, for those a class file
//! needs but the segment does not transmit, by their spelling.

use std::collections::HashMap;

use super::coding::{Bands, CHAR3, DELTA5, UDELTA5, UNSIGNED5};
use super::header::Header;
use super::{malformed, Result, Strings};

/// A string constant of a class file: a transmitted `cp_Utf8` constant, a transmitted
/// `cp_Signature` constant no `cp_Utf8` constant is spelled as, or one the segment does not
/// transmit, by its spelling.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Text {
    Pool(usize),
    Signature(usize),
    Extra(Vec<u16>),
}

/// A class constant: a transmitted `cp_Class` constant, or one the segment does not transmit,
/// by its name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum ClassRef {
    Pool(usize),
    Extra(Vec<u16>),
}

/// A constant a class file refers to, by the index in its pool of a transmitted one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Key {
    Utf8(Text),
    Int(usize),
    Float(usize),
    Long(usize),
    Double(usize),
    String(usize),
    Class(ClassRef),
    Descr(usize),
    Field(usize),
    Method(usize),
    Imethod(usize),
}

/// Where section 7.2 places a constant in a class file's constant pool: the transmitted ones in
/// `cp_All` order, then the others, strings before classes, each by its spelling as
/// `String.compareTo` orders them.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Rank {
    Transmitted(usize),
    ExtraUtf8(Vec<u16>),
    ExtraClass(Vec<u16>),
}

/// The pools of `cp_All` in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Utf8,
    Int,
    Float,
    Long,
    Double,
    String,
    Class,
    Signature,
    Descr,
    Field,
    Method,
    Imethod,
}

const KINDS: [Kind; 12] = [
    Kind::Utf8,
    Kind::Int,
    Kind::Float,
    Kind::Long,
    Kind::Double,
    Kind::String,
    Kind::Class,
    Kind::Signature,
    Kind::Descr,
    Kind::Field,
    Kind::Method,
    Kind::Imethod,
];

/// The constant pools of a segment.
#[derive(Debug, Default)]
pub(super) struct Pool {
    pub(super) utf8: Vec<Vec<u16>>,
    pub(super) ints: Vec<i32>,
    pub(super) floats: Vec<u32>,
    pub(super) longs: Vec<u64>,
    pub(super) doubles: Vec<u64>,
    /// The `cp_Utf8` spelling of each string.
    pub(super) strings: Vec<usize>,
    /// The `cp_Utf8` name of each class.
    pub(super) classes: Vec<usize>,
    /// Each signature spelled out, its class names in place.
    pub(super) signatures: Vec<Vec<u16>>,
    /// The `cp_Utf8` name and `cp_Signature` type of each descriptor.
    pub(super) descrs: Vec<(usize, usize)>,
    /// The `cp_Class` class and `cp_Descr` descriptor of each field.
    pub(super) fields: Vec<(usize, usize)>,
    pub(super) methods: Vec<(usize, usize)>,
    pub(super) imethods: Vec<(usize, usize)>,
    utf8_named: HashMap<Vec<u16>, usize>,
    signature_named: HashMap<Vec<u16>, usize>,
    class_named: HashMap<usize, usize>,
}

/// `value`, an index into a pool of `len` constants, checked.
pub(super) fn index(value: i32, len: usize, pool: &str) -> Result<usize> {
    usize::try_from(value)
        .ok()
        .filter(|&index| index < len)
        .ok_or_else(|| malformed(format!("a reference {value} past the {len} {pool} constants")))
}

impl Pool {
    /// The pools, read from the `cp_bands` the counts of `header` size, their strings counted
    /// against `strings`.
    pub(super) fn read(bands: &mut Bands, header: &Header, strings: &mut Strings) -> Result<Pool> {
        let counts = &header.pool_counts;
        let mut pool = Pool {
            utf8: read_utf8(bands, counts[0], strings)?,
            ..Pool::default()
        };
        pool.ints = bands.band(UDELTA5, counts[1])?;
        pool.floats = bands
            .band(UDELTA5, counts[2])?
            .into_iter()
            .map(|bits| bits as u32)
            .collect();
        pool.longs = read_wide(bands, counts[3])?;
        pool.doubles = read_wide(bands, counts[4])?;
        let utf8 = pool.utf8.len();
        pool.strings = references(bands.band(UDELTA5, counts[5])?, utf8, "cp_Utf8")?;
        pool.classes = references(bands.band(UDELTA5, counts[6])?, utf8, "cp_Utf8")?;
        let forms = references(bands.band(DELTA5, counts[7])?, utf8, "cp_Utf8")?;
        let class_count: usize = forms
            .iter()
            .map(|&form| pool.utf8[form].iter().filter(|&&c| c == u16::from(b'L')).count())
            .sum();
        let signature_classes =
            references(bands.band(UDELTA5, class_count)?, pool.classes.len(), "cp_Class")?;
        let mut named = signature_classes.into_iter();
        for form in forms {
            let mut signature = Vec::new();
            for &c in &pool.utf8[form] {
                signature.push(c);
                if c == u16::from(b'L') {
                    let class = named.next().expect("one class per L of the forms");
                    signature.extend_from_slice(&pool.utf8[pool.classes[class]]);
                }
            }
            strings.spend(signature.len())?;
            pool.signatures.push(signature);
        }
        let names = references(bands.band(DELTA5, counts[8])?, utf8, "cp_Utf8")?;
        let types = references(
            bands.band(UDELTA5, counts[8])?,
            pool.signatures.len(),
            "cp_Signature",
        )?;
        pool.descrs = names.into_iter().zip(types).collect();
        pool.fields = pool.read_members(bands, counts[9])?;
        pool.methods = pool.read_members(bands, counts[10])?;
        pool.imethods = pool.read_members(bands, counts[11])?;
        for (index, spelling) in pool.utf8.iter().enumerate() {
            pool.utf8_named.entry(spelling.clone()).or_insert(index);
        }
        for (index, spelling) in pool.signatures.iter().enumerate() {
            pool.signature_named
                .entry(spelling.clone())
                .or_insert(index);
        }
        for (index, &name) in pool.classes.iter().enumerate() {
            pool.class_named.entry(name).or_insert(index);
        }
        Ok(pool)
    }

    /// The class and descriptor of each of `count` fields or methods.
    fn read_members(&self, bands: &mut Bands, count: usize) -> Result<Vec<(usize, usize)>> {
        let classes = references(bands.band(DELTA5, count)?, self.classes.len(), "cp_Class")?;
        let descrs = references(bands.band(UDELTA5, count)?, self.descrs.len(), "cp_Descr")?;
        Ok(classes.into_iter().zip(descrs).collect())
    }

    /// The string constant spelled `spelling`: the `cp_Utf8` constant so spelled, else the
    /// `cp_Signature` constant so spelled, else one of its own.
    pub(super) fn text(&self, spelling: &[u16]) -> Text {
        if let Some(&index) = self.utf8_named.get(spelling) {
            Text::Pool(index)
        } else if let Some(&index) = self.signature_named.get(spelling) {
            Text::Signature(index)
        } else {
            Text::Extra(spelling.to_vec())
        }
    }

    /// The string constant of the signature `index`, flattened: the `cp_Utf8` constant spelled
    /// as it is, if there is one.
    pub(super) fn signature_text(&self, index: usize) -> Text {
        match self.utf8_named.get(&self.signatures[index]) {
            Some(&utf8) => Text::Pool(utf8),
            None => Text::Signature(index),
        }
    }

    /// The class constant named `name`: the `cp_Class` constant of that name, else one of its
    /// own.
    pub(super) fn class_named(&self, name: &[u16]) -> ClassRef {
        self.utf8_named
            .get(name)
            .and_then(|utf8| self.class_named.get(utf8))
            .map_or_else(|| ClassRef::Extra(name.to_vec()), |&index| ClassRef::Pool(index))
    }

    /// The spelling of a string constant.
    pub(super) fn spelling<'a>(&'a self, text: &'a Text) -> &'a [u16] {
        match text {
            Text::Pool(index) => &self.utf8[*index],
            Text::Signature(index) => &self.signatures[*index],
            Text::Extra(spelling) => spelling,
        }
    }

    /// The name of a class constant.
    pub(super) fn class_name<'a>(&'a self, class: &'a ClassRef) -> &'a [u16] {
        match class {
            ClassRef::Pool(index) => &self.utf8[self.classes[*index]],
            ClassRef::Extra(name) => name,
        }
    }

    /// The `cp_Utf8` name of the descriptor `descr` and the spelling of its type.
    pub(super) fn descr(&self, descr: usize) -> (usize, &[u16]) {
        let (name, signature) = self.descrs[descr];
        (name, &self.signatures[signature])
    }

    fn count(&self, kind: Kind) -> usize {
        match kind {
            Kind::Utf8 => self.utf8.len(),
            Kind::Int => self.ints.len(),
            Kind::Float => self.floats.len(),
            Kind::Long => self.longs.len(),
            Kind::Double => self.doubles.len(),
            Kind::String => self.strings.len(),
            Kind::Class => self.classes.len(),
            Kind::Signature => self.signatures.len(),
            Kind::Descr => self.descrs.len(),
            Kind::Field => self.fields.len(),
            Kind::Method => self.methods.len(),
            Kind::Imethod => self.imethods.len(),
        }
    }

    /// The place in `cp_All` of the first constant of the pool `kind`.
    fn offset(&self, kind: Kind) -> usize {
        KINDS
            .iter()
            .take_while(|&&before| before != kind)
            .map(|&before| self.count(before))
            .sum()
    }

    /// The constant at `index` in `cp_All`, as an untyped reference names it.
    pub(super) fn all(&self, value: i32) -> Result<Key> {
        let mut index = usize::try_from(value)
            .map_err(|_| malformed(format!("the negative cp_All reference {value}")))?;
        for kind in KINDS {
            let count = self.count(kind);
            if index < count {
                return Ok(match kind {
                    Kind::Utf8 => Key::Utf8(Text::Pool(index)),
                    Kind::Int => Key::Int(index),
                    Kind::Float => Key::Float(index),
                    Kind::Long => Key::Long(index),
                    Kind::Double => Key::Double(index),
                    Kind::String => Key::String(index),
                    Kind::Class => Key::Class(ClassRef::Pool(index)),
                    Kind::Signature => Key::Utf8(self.signature_text(index)),
                    Kind::Descr => Key::Descr(index),
                    Kind::Field => Key::Field(index),
                    Kind::Method => Key::Method(index),
                    Kind::Imethod => Key::Imethod(index),
                });
            }
            index -= count;
        }
        Err(malformed(format!(
            "the cp_All reference {value} past the constants"
        )))
    }

    /// Where `key` goes in a class file's constant pool, before the operands of `ldc` are moved
    /// to its front.
    pub(super) fn rank(&self, key: &Key) -> Rank {
        let (kind, index) = match key {
            Key::Utf8(Text::Pool(index)) => (Kind::Utf8, *index),
            Key::Utf8(Text::Signature(index)) => (Kind::Signature, *index),
            Key::Utf8(Text::Extra(spelling)) => return Rank::ExtraUtf8(spelling.clone()),
            Key::Int(index) => (Kind::Int, *index),
            Key::Float(index) => (Kind::Float, *index),
            Key::Long(index) => (Kind::Long, *index),
            Key::Double(index) => (Kind::Double, *index),
            Key::String(index) => (Kind::String, *index),
            Key::Class(ClassRef::Pool(index)) => (Kind::Class, *index),
            Key::Class(ClassRef::Extra(name)) => return Rank::ExtraClass(name.clone()),
            Key::Descr(index) => (Kind::Descr, *index),
            Key::Field(index) => (Kind::Field, *index),
            Key::Method(index) => (Kind::Method, *index),
            Key::Imethod(index) => (Kind::Imethod, *index),
        };
        Rank::Transmitted(self.offset(kind) + index)
    }

    /// The constants `key` refers to, which a class file that refers to it holds too.
    pub(super) fn referred(&self, key: &Key) -> Vec<Key> {
        let member = |(class, descr): (usize, usize)| {
            vec![Key::Class(ClassRef::Pool(class)), Key::Descr(descr)]
        };
        match key {
            Key::String(index) => vec![Key::Utf8(Text::Pool(self.strings[*index]))],
            Key::Class(ClassRef::Pool(index)) => {
                vec![Key::Utf8(Text::Pool(self.classes[*index]))]
            }
            Key::Class(ClassRef::Extra(name)) => vec![Key::Utf8(self.text(name))],
            Key::Descr(index) => {
                let (name, signature) = self.descrs[*index];
                vec![
                    Key::Utf8(Text::Pool(name)),
                    Key::Utf8(self.signature_text(signature)),
                ]
            }
            Key::Field(index) => member(self.fields[*index]),
            Key::Method(index) => member(self.methods[*index]),
            Key::Imethod(index) => member(self.imethods[*index]),
            Key::Utf8(_) | Key::Int(_) | Key::Float(_) | Key::Long(_) | Key::Double(_) => {
                Vec::new()
            }
        }
    }
}

/// The values of a band of references into a pool of `len` constants, checked.
pub(super) fn references(values: Vec<i32>, len: usize, pool: &str) -> Result<Vec<usize>> {
    values
        .into_iter()
        .map(|value| index(value, len, pool))
        .collect()
}

/// The 64-bit values of `count` constants, their high words in a band of their own before the
/// band of their low words.
fn read_wide(bands: &mut Bands, count: usize) -> Result<Vec<u64>> {
    let high = bands.band(UDELTA5, count)?;
    let low = bands.band(DELTA5, count)?;
    Ok(high
        .into_iter()
        .zip(low)
        .map(|(high, low)| u64::from(high as u32) << 32 | u64::from(low as u32))
        .collect())
}

/// The `count` strings of `cp_Utf8` (section 5.3.2): the empty string, then each made of a
/// prefix of the one before and a suffix, small ones in one band and big ones each in a band of
/// its own.
fn read_utf8(bands: &mut Bands, count: usize, strings: &mut Strings) -> Result<Vec<Vec<u16>>> {
    let prefixes = bands.counts(DELTA5, count.saturating_sub(2))?;
    let suffixes = bands.counts(UNSIGNED5, count.saturating_sub(1))?;
    let char_count = suffixes
        .iter()
        .try_fold(0usize, |sum, &suffix| sum.checked_add(suffix))
        .ok_or_else(|| malformed("the cp_Utf8 suffixes overflow"))?;
    let chars = bands.band(CHAR3, char_count)?;
    let big_count = suffixes.iter().filter(|&&suffix| suffix == 0).count();
    let big_suffixes = bands.counts(DELTA5, big_count)?;
    let mut big_chars = Vec::with_capacity(big_suffixes.len());
    for &length in &big_suffixes {
        big_chars.push(bands.band(DELTA5, length)?);
    }
    let mut utf8: Vec<Vec<u16>> = Vec::with_capacity(count);
    if count == 0 {
        return Ok(utf8);
    }
    utf8.push(Vec::new());
    let (mut small, mut big) = (chars.into_iter(), big_chars.into_iter());
    for (at, &suffix) in suffixes.iter().enumerate() {
        let prefix = if at == 0 { 0 } else { prefixes[at - 1] };
        let before = &utf8[at];
        if prefix > before.len() {
            return Err(malformed(format!(
                "a cp_Utf8 prefix of {prefix} characters of a string of {}",
                before.len()
            )));
        }
        let mut string = before[..prefix].to_vec();
        if suffix == 0 {
            let chars = big.next().expect("one big suffix per empty small suffix");
            string.extend(chars.into_iter().map(|c| c as u16));
        } else {
            string.extend(small.by_ref().take(suffix).map(|c| c as u16));
        }
        strings.spend(string.len())?;
        utf8.push(string);
    }
    Ok(utf8)
}
