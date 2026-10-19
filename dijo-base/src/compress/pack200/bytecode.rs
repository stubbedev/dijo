//! The bytecode bands (JSR 200 section 5.10): the packed bytecodes of every code of a
//! segment and their operands, read as commons-compress's `BcBands.read` reads them and
//! dealt to the instructions in the order its `OperandManager` deals them.

use std::collections::HashMap;
use std::vec::IntoIter;

use super::classes::Class;
use super::coding::{Bands, BRANCH5, BYTE1, DELTA5, UNSIGNED5};
use super::pool::{index, ClassRef, Key, Pool};
use super::{malformed, Result};

/// The packed opcode that ends the bytecodes of a code.
const END: u8 = 0xFF;

/// The bytecodes of a `Code` attribute, as packed.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Bytecode {
    pub(super) instructions: Vec<Instruction>,
}

/// An instruction as packed: its packed opcode and its operand, taken from the bands.
///
/// Opcodes below 202 are those of the class file. The others stand for: 202 to 205
/// `getstatic`, `putstatic`, `getfield` and `putfield` of a field of the class itself, 206
/// to 208 `invokevirtual`, `invokespecial` and `invokestatic` of a method of the class
/// itself, 209 to 215 the same seven preceded by `aload_0`, 216 to 229 the fourteen of the
/// super class likewise, 230 to 232 `invokespecial` of an `<init>` method of the class, its
/// super class or the class of the latest `new`, 233 `ldc` of a class, 234 and 235 `ldc` of
/// an int and a float, 236 to 238 the same as `ldc_w`, 239 `ldc2_w` of a double (20 being
/// `ldc2_w` of a long, and 18 and 19 `ldc` and `ldc_w` of a string), 253 a reference escape
/// and 254 a byte escape.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Instruction {
    pub(super) opcode: u8,
    /// Whether `wide` precedes it.
    pub(super) wide: bool,
    pub(super) operand: Operand,
}

/// The operand of an instruction, its values as transmitted. Labels are the `bc_label`
/// values, relative to the instruction, which the rebuilt class file renumbers.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Operand {
    None,
    /// The `bc_byte` of `bipush` and `newarray`.
    Byte(i32),
    /// The `bc_short` of `sipush`.
    Short(i32),
    /// The `bc_local` of a load, a store or `ret`.
    Local(i32),
    /// The local and the constant of `iinc`, the constant a `bc_byte`, or a `bc_short` where
    /// `wide` precedes it.
    Iinc {
        local: i32,
        constant: i32,
    },
    /// The target of a jump.
    Label(i32),
    /// `tableswitch`: the default target, the lowest case and the target of each case.
    TableSwitch {
        default: i32,
        low: i32,
        labels: Vec<i32>,
    },
    /// `lookupswitch`: the default target and each case with its target.
    LookupSwitch {
        default: i32,
        cases: Vec<(i32, i32)>,
    },
    /// The constant a reference names, the class itself for a `bc_classref` of zero.
    Ref(Key),
    /// `multianewarray`: the class and the dimensions.
    MultiArray {
        class: Key,
        dimensions: i32,
    },
    /// A reference escape: the constant, written in `size` bytes.
    RefEscape {
        key: Key,
        size: i32,
    },
    /// A byte escape: the bytes, written as they are.
    ByteEscape(Vec<u8>),
}

/// What operand bands an opcode takes values from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    None,
    Byte,
    Short,
    Local,
    Iinc,
    WideIinc,
    Label,
    TableSwitch,
    LookupSwitch,
    IntRef,
    FloatRef,
    LongRef,
    DoubleRef,
    StringRef,
    ClassRef,
    NewClassRef,
    MultiArray,
    FieldRef,
    MethodRef,
    ImethodRef,
    ThisField,
    SuperField,
    ThisMethod,
    SuperMethod,
    ThisInit,
    SuperInit,
    NewInit,
    RefEscape,
    ByteEscape,
}

/// The shape of the packed opcode `opcode`, as `BcBands.read` counts its operands.
fn shape(opcode: u8) -> Shape {
    match opcode {
        16 | 188 => Shape::Byte,
        17 => Shape::Short,
        18 | 19 => Shape::StringRef,
        234 | 237 => Shape::IntRef,
        235 | 238 => Shape::FloatRef,
        20 => Shape::LongRef,
        239 => Shape::DoubleRef,
        197 => Shape::MultiArray,
        187 => Shape::NewClassRef,
        233 | 236 | 189 | 192 | 193 => Shape::ClassRef,
        169 | 21..=25 | 54..=58 => Shape::Local,
        167 | 168 | 200 | 201 | 153..=166 | 198 | 199 => Shape::Label,
        170 => Shape::TableSwitch,
        171 => Shape::LookupSwitch,
        178..=181 => Shape::FieldRef,
        182..=184 => Shape::MethodRef,
        185 => Shape::ImethodRef,
        202..=205 | 209..=212 => Shape::ThisField,
        206..=208 | 213..=215 => Shape::ThisMethod,
        216..=219 | 223..=226 => Shape::SuperField,
        220..=222 | 227..=229 => Shape::SuperMethod,
        132 => Shape::Iinc,
        230 => Shape::ThisInit,
        231 => Shape::SuperInit,
        232 => Shape::NewInit,
        253 => Shape::RefEscape,
        254 => Shape::ByteEscape,
        _ => Shape::None,
    }
}

/// The shape of the opcode `opcode` after `wide`: `iinc` takes a `bc_short` constant, loads,
/// stores and `ret` a local, and the others nothing, as in `BcBands.read`.
fn wide_shape(opcode: u8) -> Shape {
    match opcode {
        132 => Shape::WideIinc,
        169 | 21..=25 | 54..=58 => Shape::Local,
        _ => Shape::None,
    }
}

/// An instruction before its operands are dealt.
struct Packed {
    opcode: u8,
    wide: bool,
    shape: Shape,
}

/// The packed instructions of the next code: the `bc_codes` up to the end marker, read
/// straight from the stream as BYTE1 values.
fn read_packed(bands: &mut Bands) -> Result<Vec<Packed>> {
    let mut packed = Vec::new();
    loop {
        let opcode = bands.input.byte()?;
        if opcode == END {
            return Ok(packed);
        }
        packed.push(if opcode == 196 {
            let widened = bands.input.byte()?;
            if widened == END {
                return Err(malformed("the bytecodes of a code end after wide"));
            }
            Packed {
                opcode: widened,
                wide: true,
                shape: wide_shape(widened),
            }
        } else {
            Packed {
                opcode,
                wide: false,
                shape: shape(opcode),
            }
        });
    }
}

/// How many values each operand band holds.
#[derive(Debug, Default)]
struct Counts {
    case_count: usize,
    byte: usize,
    short: usize,
    local: usize,
    label: usize,
    intref: usize,
    floatref: usize,
    longref: usize,
    doubleref: usize,
    stringref: usize,
    classref: usize,
    fieldref: usize,
    methodref: usize,
    imethodref: usize,
    thisfield: usize,
    superfield: usize,
    thismethod: usize,
    supermethod: usize,
    initref: usize,
    escref: usize,
    escsize: usize,
}

impl Counts {
    /// Counts the operands of an instruction of the shape `shape`, the labels of a switch
    /// but its default one counted once the case counts are read.
    fn count(&mut self, shape: Shape) {
        match shape {
            Shape::None => {}
            Shape::Byte => self.byte += 1,
            Shape::Short => self.short += 1,
            Shape::Local => self.local += 1,
            Shape::Iinc => {
                self.local += 1;
                self.byte += 1;
            }
            Shape::WideIinc => {
                self.local += 1;
                self.short += 1;
            }
            Shape::Label => self.label += 1,
            Shape::TableSwitch | Shape::LookupSwitch => {
                self.case_count += 1;
                self.label += 1;
            }
            Shape::IntRef => self.intref += 1,
            Shape::FloatRef => self.floatref += 1,
            Shape::LongRef => self.longref += 1,
            Shape::DoubleRef => self.doubleref += 1,
            Shape::StringRef => self.stringref += 1,
            Shape::ClassRef | Shape::NewClassRef => self.classref += 1,
            Shape::MultiArray => {
                self.classref += 1;
                self.byte += 1;
            }
            Shape::FieldRef => self.fieldref += 1,
            Shape::MethodRef => self.methodref += 1,
            Shape::ImethodRef => self.imethodref += 1,
            Shape::ThisField => self.thisfield += 1,
            Shape::SuperField => self.superfield += 1,
            Shape::ThisMethod => self.thismethod += 1,
            Shape::SuperMethod => self.supermethod += 1,
            Shape::ThisInit | Shape::SuperInit | Shape::NewInit => self.initref += 1,
            Shape::RefEscape => self.escref += 1,
            Shape::ByteEscape => self.escsize += 1,
        }
    }
}

/// The values of a band, dealt in order.
struct Queue(IntoIter<i32>);

impl Queue {
    fn new(values: Vec<i32>) -> Self {
        Self(values.into_iter())
    }

    /// The next value, which the counts the band was read with provide.
    fn next(&mut self) -> i32 {
        self.0.next().expect("a value per counted operand")
    }
}

/// The operand bands of a segment, in the order of section 5.10.
struct Operands {
    case_count: Queue,
    case_value: Queue,
    byte: Queue,
    short: Queue,
    local: Queue,
    label: Queue,
    intref: Queue,
    floatref: Queue,
    longref: Queue,
    doubleref: Queue,
    stringref: Queue,
    classref: Queue,
    fieldref: Queue,
    methodref: Queue,
    imethodref: Queue,
    thisfield: Queue,
    superfield: Queue,
    thismethod: Queue,
    supermethod: Queue,
    initref: Queue,
    escref: Queue,
    escrefsize: Queue,
    escsize: Queue,
    escbyte: Queue,
}

impl Operands {
    /// The operand bands of the instructions `codes`.
    fn read(bands: &mut Bands, codes: &[Vec<Packed>]) -> Result<Operands> {
        let mut counts = Counts::default();
        let mut tables = Vec::new();
        for packed in codes.iter().flatten() {
            counts.count(packed.shape);
            match packed.shape {
                Shape::TableSwitch => tables.push(true),
                Shape::LookupSwitch => tables.push(false),
                _ => {}
            }
        }
        let case_counts = bands.counts(UNSIGNED5, counts.case_count)?;
        let mut case_values = 0usize;
        for (&case_count, &table) in case_counts.iter().zip(&tables) {
            case_values = case_values
                .checked_add(if table { 1 } else { case_count })
                .ok_or_else(|| malformed("the switch case counts overflow"))?;
            counts.label = counts
                .label
                .checked_add(case_count)
                .ok_or_else(|| malformed("the switch case counts overflow"))?;
        }
        let case_value = bands.band(DELTA5, case_values)?;
        let byte = bands.band(BYTE1, counts.byte)?;
        let short = bands.band(DELTA5, counts.short)?;
        let local = bands.band(UNSIGNED5, counts.local)?;
        let label = bands.band(BRANCH5, counts.label)?;
        let intref = bands.band(DELTA5, counts.intref)?;
        let floatref = bands.band(DELTA5, counts.floatref)?;
        let longref = bands.band(DELTA5, counts.longref)?;
        let doubleref = bands.band(DELTA5, counts.doubleref)?;
        let stringref = bands.band(DELTA5, counts.stringref)?;
        let classref = bands.band(UNSIGNED5, counts.classref)?;
        let fieldref = bands.band(DELTA5, counts.fieldref)?;
        let methodref = bands.band(UNSIGNED5, counts.methodref)?;
        let imethodref = bands.band(DELTA5, counts.imethodref)?;
        let thisfield = bands.band(UNSIGNED5, counts.thisfield)?;
        let superfield = bands.band(UNSIGNED5, counts.superfield)?;
        let thismethod = bands.band(UNSIGNED5, counts.thismethod)?;
        let supermethod = bands.band(UNSIGNED5, counts.supermethod)?;
        let initref = bands.band(UNSIGNED5, counts.initref)?;
        let escref = bands.band(UNSIGNED5, counts.escref)?;
        let escrefsize = bands.band(UNSIGNED5, counts.escref)?;
        let escsizes = bands.counts(UNSIGNED5, counts.escsize)?;
        let escbytes = escsizes
            .iter()
            .try_fold(0usize, |sum, &size| sum.checked_add(size))
            .ok_or_else(|| malformed("the byte escape sizes overflow"))?;
        let escbyte = bands.band(BYTE1, escbytes)?;
        Ok(Operands {
            case_count: Queue::new(case_counts.into_iter().map(|count| count as i32).collect()),
            case_value: Queue::new(case_value),
            byte: Queue::new(byte),
            short: Queue::new(short),
            local: Queue::new(local),
            label: Queue::new(label),
            intref: Queue::new(intref),
            floatref: Queue::new(floatref),
            longref: Queue::new(longref),
            doubleref: Queue::new(doubleref),
            stringref: Queue::new(stringref),
            classref: Queue::new(classref),
            fieldref: Queue::new(fieldref),
            methodref: Queue::new(methodref),
            imethodref: Queue::new(imethodref),
            thisfield: Queue::new(thisfield),
            superfield: Queue::new(superfield),
            thismethod: Queue::new(thismethod),
            supermethod: Queue::new(supermethod),
            initref: Queue::new(initref),
            escref: Queue::new(escref),
            escrefsize: Queue::new(escrefsize),
            escsize: Queue::new(escsizes.into_iter().map(|size| size as i32).collect()),
            escbyte: Queue::new(escbyte),
        })
    }
}

/// The fields, methods and `<init>` methods of each class, by the name of the class, as
/// commons-compress's `SegmentConstantPool` matches the class-specific references.
struct Members<'p> {
    fields: HashMap<&'p [u16], Vec<usize>>,
    methods: HashMap<&'p [u16], Vec<usize>>,
    inits: HashMap<&'p [u16], Vec<usize>>,
}

impl<'p> Members<'p> {
    fn new(pool: &'p Pool) -> Self {
        let class_name = |class: usize| pool.utf8[pool.classes[class]].as_slice();
        let init: Vec<u16> = "<init>".encode_utf16().collect();
        let mut members = Members {
            fields: HashMap::new(),
            methods: HashMap::new(),
            inits: HashMap::new(),
        };
        for (at, &(class, _)) in pool.fields.iter().enumerate() {
            members
                .fields
                .entry(class_name(class))
                .or_default()
                .push(at);
        }
        for (at, &(class, descr)) in pool.methods.iter().enumerate() {
            members
                .methods
                .entry(class_name(class))
                .or_default()
                .push(at);
            if pool.utf8[pool.descr(descr).0] == init {
                members.inits.entry(class_name(class)).or_default().push(at);
            }
        }
        members
    }

    /// The member at `value` among those of `map` of the class `class`.
    fn nth(
        map: &HashMap<&[u16], Vec<usize>>,
        class: &[u16],
        value: i32,
        what: &str,
    ) -> Result<usize> {
        let members = map.get(class).map_or(&[][..], Vec::as_slice);
        Ok(members[index(value, members.len(), what)?])
    }
}

/// Reads the `bc_bands` into the codes of `classes`, whose methods with a `Code` attribute
/// have the code bands read, in the order of the classes and their methods.
pub(super) fn read_bytecodes(bands: &mut Bands, pool: &Pool, classes: &mut [Class]) -> Result<()> {
    let mut codes = Vec::new();
    for class in classes.iter() {
        for _ in class.methods.iter().filter(|method| method.code.is_some()) {
            codes.push(read_packed(bands)?);
        }
    }
    let mut operands = Operands::read(bands, &codes)?;
    let members = Members::new(pool);
    let mut new_class: Option<usize> = None;
    let mut codes = codes.into_iter();
    for class in classes.iter_mut() {
        let this = class.this;
        let super_class = class.super_class.unwrap_or(this);
        let name = |class: usize| pool.utf8[pool.classes[class]].as_slice();
        for code in class
            .methods
            .iter_mut()
            .filter_map(|method| method.code.as_mut())
        {
            let packed = codes.next().expect("packed bytecodes per code");
            let mut instructions = Vec::with_capacity(packed.len());
            for Packed {
                opcode,
                wide,
                shape,
            } in packed
            {
                let ops = &mut operands;
                let class_ref = |value: i32| -> Result<usize> {
                    match value {
                        0 => Ok(this),
                        value => index(value - 1, pool.classes.len(), "cp_Class"),
                    }
                };
                let operand = match shape {
                    Shape::None => Operand::None,
                    Shape::Byte => Operand::Byte(ops.byte.next()),
                    Shape::Short => Operand::Short(ops.short.next()),
                    Shape::Local => Operand::Local(ops.local.next()),
                    Shape::Iinc => Operand::Iinc {
                        local: ops.local.next(),
                        constant: ops.byte.next(),
                    },
                    Shape::WideIinc => Operand::Iinc {
                        local: ops.local.next(),
                        constant: ops.short.next(),
                    },
                    Shape::Label => Operand::Label(ops.label.next()),
                    Shape::TableSwitch => {
                        let count = ops.case_count.next() as usize;
                        let default = ops.label.next();
                        let low = ops.case_value.next();
                        let labels = (0..count).map(|_| ops.label.next()).collect();
                        Operand::TableSwitch {
                            default,
                            low,
                            labels,
                        }
                    }
                    Shape::LookupSwitch => {
                        let count = ops.case_count.next() as usize;
                        let default = ops.label.next();
                        let values: Vec<i32> = (0..count).map(|_| ops.case_value.next()).collect();
                        let cases = values
                            .into_iter()
                            .map(|value| (value, ops.label.next()))
                            .collect();
                        Operand::LookupSwitch { default, cases }
                    }
                    Shape::IntRef => Operand::Ref(Key::Int(index(
                        ops.intref.next(),
                        pool.ints.len(),
                        "cp_Int",
                    )?)),
                    Shape::FloatRef => Operand::Ref(Key::Float(index(
                        ops.floatref.next(),
                        pool.floats.len(),
                        "cp_Float",
                    )?)),
                    Shape::LongRef => Operand::Ref(Key::Long(index(
                        ops.longref.next(),
                        pool.longs.len(),
                        "cp_Long",
                    )?)),
                    Shape::DoubleRef => Operand::Ref(Key::Double(index(
                        ops.doubleref.next(),
                        pool.doubles.len(),
                        "cp_Double",
                    )?)),
                    Shape::StringRef => Operand::Ref(Key::String(index(
                        ops.stringref.next(),
                        pool.strings.len(),
                        "cp_String",
                    )?)),
                    Shape::ClassRef => {
                        Operand::Ref(Key::Class(ClassRef::Pool(class_ref(ops.classref.next())?)))
                    }
                    Shape::NewClassRef => {
                        let class = class_ref(ops.classref.next())?;
                        new_class = Some(class);
                        Operand::Ref(Key::Class(ClassRef::Pool(class)))
                    }
                    Shape::MultiArray => Operand::MultiArray {
                        class: Key::Class(ClassRef::Pool(class_ref(ops.classref.next())?)),
                        dimensions: ops.byte.next(),
                    },
                    Shape::FieldRef => Operand::Ref(Key::Field(index(
                        ops.fieldref.next(),
                        pool.fields.len(),
                        "cp_Field",
                    )?)),
                    Shape::MethodRef => Operand::Ref(Key::Method(index(
                        ops.methodref.next(),
                        pool.methods.len(),
                        "cp_Method",
                    )?)),
                    Shape::ImethodRef => Operand::Ref(Key::Imethod(index(
                        ops.imethodref.next(),
                        pool.imethods.len(),
                        "cp_Imethod",
                    )?)),
                    Shape::ThisField => Operand::Ref(Key::Field(Members::nth(
                        &members.fields,
                        name(this),
                        ops.thisfield.next(),
                        "fields of the class",
                    )?)),
                    Shape::SuperField => Operand::Ref(Key::Field(Members::nth(
                        &members.fields,
                        name(super_class),
                        ops.superfield.next(),
                        "fields of the super class",
                    )?)),
                    Shape::ThisMethod => Operand::Ref(Key::Method(Members::nth(
                        &members.methods,
                        name(this),
                        ops.thismethod.next(),
                        "methods of the class",
                    )?)),
                    Shape::SuperMethod => Operand::Ref(Key::Method(Members::nth(
                        &members.methods,
                        name(super_class),
                        ops.supermethod.next(),
                        "methods of the super class",
                    )?)),
                    Shape::ThisInit | Shape::SuperInit | Shape::NewInit => {
                        let owner = match shape {
                            Shape::ThisInit => this,
                            Shape::SuperInit => super_class,
                            _ => new_class.ok_or_else(|| {
                                malformed("an invokespecial_new_init before any new")
                            })?,
                        };
                        Operand::Ref(Key::Method(Members::nth(
                            &members.inits,
                            name(owner),
                            ops.initref.next(),
                            "<init> methods of the class",
                        )?))
                    }
                    Shape::RefEscape => Operand::RefEscape {
                        key: pool.all(ops.escref.next())?,
                        size: ops.escrefsize.next(),
                    },
                    Shape::ByteEscape => {
                        let size = ops.escsize.next() as usize;
                        Operand::ByteEscape((0..size).map(|_| ops.escbyte.next() as u8).collect())
                    }
                };
                instructions.push(Instruction {
                    opcode,
                    wide,
                    operand,
                });
            }
            code.bytecode = Some(Bytecode { instructions });
        }
    }
    Ok(())
}
