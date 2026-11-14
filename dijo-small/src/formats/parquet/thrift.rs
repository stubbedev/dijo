//! Thrift's compact protocol, in which Parquet writes its footer and its page headers: a struct
//! is its fields, each headed by the difference of its id from the last one's and its type, up
//! to a stop byte; integers are zigzag varints, binaries and strings a varint length and their
//! bytes, lists a header of their size and their elements' type.
//!
//! A struct is read in place, its fields handed to the reader of the struct as they come, and
//! those it does not know skipped. Nesting is bounded by [`MAX_DEPTH`], and no size is believed
//! beyond the bytes left: every element of a list takes at least a byte.

use super::{bits, Problem};

/// The deepest structs and containers nest; Parquet's own nest six deep.
pub const MAX_DEPTH: usize = 64;

/// The type of a field or of a list's elements, by its compact code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    /// A boolean: in a struct, a field of this code is true; in a container, an element of either
    /// boolean code is a byte.
    True,
    False,
    Byte,
    I16,
    I32,
    I64,
    Double,
    Binary,
    List,
    Set,
    Map,
    Struct,
    Uuid,
}

impl Type {
    fn of(code: u8, at: usize) -> Result<Self, Problem> {
        Ok(match code {
            1 => Self::True,
            2 => Self::False,
            3 => Self::Byte,
            4 => Self::I16,
            5 => Self::I32,
            6 => Self::I64,
            7 => Self::Double,
            8 => Self::Binary,
            9 => Self::List,
            10 => Self::Set,
            11 => Self::Map,
            12 => Self::Struct,
            13 => Self::Uuid,
            _ => {
                return Err(Problem::Malformed(format!(
                    "the Thrift type {code} at byte {at}, which the compact protocol does not \
                     define"
                )))
            }
        })
    }
}

/// A field of a struct: its id and its type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    pub id: i16,
    pub kind: Type,
}

/// A reader of compact Thrift in `input`, from a position in it.
#[derive(Debug)]
pub struct Reader<'a> {
    input: &'a [u8],
    at: usize,
    depth: usize,
}

impl<'a> Reader<'a> {
    pub fn new(input: &'a [u8], at: usize) -> Self {
        Self {
            input,
            at,
            depth: 0,
        }
    }

    /// Where the next value starts.
    pub fn position(&self) -> usize {
        self.at
    }

    fn left(&self) -> usize {
        self.input.len().saturating_sub(self.at)
    }

    fn ends(&self, what: &str) -> Problem {
        Problem::Malformed(format!(
            "the Thrift data ends inside {what} at byte {}",
            self.at
        ))
    }

    fn byte(&mut self) -> Result<u8, Problem> {
        let byte = *self
            .input
            .get(self.at)
            .ok_or_else(|| self.ends("a value"))?;
        self.at += 1;
        Ok(byte)
    }

    fn take(&mut self, length: usize, what: &str) -> Result<&'a [u8], Problem> {
        if length > self.left() {
            return Err(self.ends(what));
        }
        let bytes = &self.input[self.at..self.at + length];
        self.at += length;
        Ok(bytes)
    }

    fn varint(&mut self) -> Result<u64, Problem> {
        bits::varint(self.input, &mut self.at)
    }

    fn zigzag(&mut self) -> Result<i64, Problem> {
        bits::zigzag(self.input, &mut self.at)
    }

    fn mismatch(&self, field: Field, expected: &str) -> Problem {
        Problem::Malformed(format!(
            "the Thrift field {} before byte {} is a {:?}, not a {expected}",
            field.id, self.at, field.kind
        ))
    }

    /// Reads the fields of the struct that starts here, handing each to `read`, which reads or
    /// skips its value.
    pub fn read_struct(
        &mut self,
        mut read: impl FnMut(&mut Self, Field) -> Result<(), Problem>,
    ) -> Result<(), Problem> {
        self.enter()?;
        let mut last = 0i16;
        loop {
            let at = self.at;
            let header = self.byte()?;
            if header == 0 {
                break;
            }
            let id = match header >> 4 {
                0 => {
                    let id = self.zigzag()?;
                    i16::try_from(id).map_err(|_| {
                        Problem::Malformed(format!("the Thrift field id {id} at byte {at}"))
                    })?
                }
                delta => last.checked_add(i16::from(delta)).ok_or_else(|| {
                    Problem::Malformed(format!("the Thrift field id past 32767 at byte {at}"))
                })?,
            };
            last = id;
            let kind = Type::of(header & 0x0F, at)?;
            read(self, Field { id, kind })?;
        }
        self.depth -= 1;
        Ok(())
    }

    fn enter(&mut self) -> Result<(), Problem> {
        if self.depth == MAX_DEPTH {
            return Err(Problem::Malformed(format!(
                "Thrift structs and containers nest deeper than {MAX_DEPTH} at byte {}",
                self.at
            )));
        }
        self.depth += 1;
        Ok(())
    }

    /// The struct of `field`, read by `read` as [`Reader::read_struct`] reads one.
    pub fn structure(
        &mut self,
        field: Field,
        read: impl FnMut(&mut Self, Field) -> Result<(), Problem>,
    ) -> Result<(), Problem> {
        if field.kind != Type::Struct {
            return Err(self.mismatch(field, "struct"));
        }
        self.read_struct(read)
    }

    /// The elements of the list of `field`, each handed with its type to `read`, which reads or
    /// skips it.
    pub fn list(
        &mut self,
        field: Field,
        mut read: impl FnMut(&mut Self, Type) -> Result<(), Problem>,
    ) -> Result<(), Problem> {
        if !matches!(field.kind, Type::List | Type::Set) {
            return Err(self.mismatch(field, "list"));
        }
        let (size, kind) = self.list_header()?;
        self.enter()?;
        for _ in 0..size {
            read(self, kind)?;
        }
        self.depth -= 1;
        Ok(())
    }

    /// The size and element type of the list that starts here, its size no more than the bytes
    /// left.
    fn list_header(&mut self) -> Result<(usize, Type), Problem> {
        let at = self.at;
        let header = self.byte()?;
        let size = match header >> 4 {
            15 => self.varint()?,
            size => u64::from(size),
        };
        let kind = Type::of(header & 0x0F, at)?;
        self.bounded(size, 1, at).map(|size| (size, kind))
    }

    /// `size` items of at least `each` bytes, if that many bytes are left.
    fn bounded(&self, size: u64, each: u64, at: usize) -> Result<usize, Problem> {
        match usize::try_from(size.saturating_mul(each)) {
            Ok(needed) if needed <= self.left() => Ok(size as usize),
            _ => Err(Problem::Malformed(format!(
                "the Thrift container at byte {at} holds {size} items, more than the {} bytes \
                 left",
                self.left()
            ))),
        }
    }

    pub fn bool(&mut self, field: Field) -> Result<bool, Problem> {
        match field.kind {
            Type::True => Ok(true),
            Type::False => Ok(false),
            _ => Err(self.mismatch(field, "bool")),
        }
    }

    pub fn i8(&mut self, field: Field) -> Result<i8, Problem> {
        if field.kind != Type::Byte {
            return Err(self.mismatch(field, "byte"));
        }
        Ok(self.byte()? as i8)
    }

    pub fn i32(&mut self, field: Field) -> Result<i32, Problem> {
        if field.kind != Type::I32 {
            return Err(self.mismatch(field, "i32"));
        }
        self.i32_value()
    }

    /// An i32 element of a list, whose type is `kind`.
    pub fn i32_element(&mut self, kind: Type) -> Result<i32, Problem> {
        if kind != Type::I32 {
            return Err(Problem::Malformed(format!(
                "a Thrift list before byte {} holds {kind:?} elements, not i32",
                self.at
            )));
        }
        self.i32_value()
    }

    fn i32_value(&mut self) -> Result<i32, Problem> {
        let at = self.at;
        let value = self.zigzag()?;
        i32::try_from(value).map_err(|_| {
            Problem::Malformed(format!(
                "the Thrift i32 at byte {at} is {value}, out of range"
            ))
        })
    }

    pub fn i64(&mut self, field: Field) -> Result<i64, Problem> {
        if field.kind != Type::I64 {
            return Err(self.mismatch(field, "i64"));
        }
        self.zigzag()
    }

    pub fn binary(&mut self, field: Field) -> Result<&'a [u8], Problem> {
        if field.kind != Type::Binary {
            return Err(self.mismatch(field, "binary"));
        }
        self.binary_value()
    }

    fn binary_value(&mut self) -> Result<&'a [u8], Problem> {
        let at = self.at;
        let length = self.varint()?;
        let length = usize::try_from(length)
            .ok()
            .filter(|&length| length <= self.left())
            .ok_or_else(|| {
                Problem::Malformed(format!(
                    "the Thrift binary at byte {at} is {length} bytes long, more than the {} \
                     left",
                    self.left()
                ))
            })?;
        self.take(length, "a binary")
    }

    /// The string of `field`, its bytes decoded as UTF-8, each malformed sequence replaced, as
    /// Thrift's Java library decodes them.
    pub fn string(&mut self, field: Field) -> Result<String, Problem> {
        self.binary(field)
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
    }

    /// Passes over the value of `field`.
    pub fn skip(&mut self, field: Field) -> Result<(), Problem> {
        match field.kind {
            Type::True | Type::False => Ok(()),
            kind => self.skip_value(kind),
        }
    }

    /// Passes over a value of `kind` in a container or a struct, a boolean taking a byte.
    pub fn skip_value(&mut self, kind: Type) -> Result<(), Problem> {
        match kind {
            Type::True | Type::False | Type::Byte => self.byte().map(drop),
            Type::I16 | Type::I32 | Type::I64 => self.varint().map(drop),
            Type::Double => self.take(8, "a double").map(drop),
            Type::Uuid => self.take(16, "a UUID").map(drop),
            Type::Binary => self.binary_value().map(drop),
            Type::List | Type::Set => {
                let (size, element) = self.list_header()?;
                self.enter()?;
                for _ in 0..size {
                    self.skip_value(element)?;
                }
                self.depth -= 1;
                Ok(())
            }
            Type::Map => {
                let at = self.at;
                let size = self.varint()?;
                if size == 0 {
                    return Ok(());
                }
                let types = self.byte()?;
                let (key, value) = (Type::of(types >> 4, at)?, Type::of(types & 0x0F, at)?);
                let size = self.bounded(size, 2, at)?;
                self.enter()?;
                for _ in 0..size {
                    self.skip_value(key)?;
                    self.skip_value(value)?;
                }
                self.depth -= 1;
                Ok(())
            }
            Type::Struct => self.read_struct(|reader, field| reader.skip(field)),
        }
    }
}
