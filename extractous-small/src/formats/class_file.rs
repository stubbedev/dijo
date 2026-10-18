//! Java class files, as Tika's `ClassParser` reads them with ASM's `ClassReader` into its
//! `XHTMLClassVisitor`: the class's declaration, its fields and its methods, without their
//! code, as Java source in a `pre` element, and the class's simple name as the title.
//!
//! The port reads class files whose structures are whole and well-formed, where ASM reads
//! every part the visitor sees without throwing. A malformed one, which ASM reads only up to
//! where it throws, after the visitor has written part of the document, stays on Tika's path;
//! one of a version newer than ASM reads fails before anything is written, as in Tika's.

use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Parser, Prepared, Unported};
use crate::java::dtoa::{java_double_to_string, java_float_to_string};

/// `Opcodes.V27`, the newest class file version ASM 9.10.1 reads.
const NEWEST_VERSION: u16 = 71;
const ACC_PUBLIC: u32 = 0x0001;
const ACC_PRIVATE: u32 = 0x0002;
const ACC_PROTECTED: u32 = 0x0004;
const ACC_STATIC: u32 = 0x0008;
const ACC_FINAL: u32 = 0x0010;
const ACC_SYNCHRONIZED: u32 = 0x0020;
const ACC_VOLATILE: u32 = 0x0040;
const ACC_TRANSIENT: u32 = 0x0080;
const ACC_NATIVE: u32 = 0x0100;
const ACC_INTERFACE: u32 = 0x0200;
const ACC_ABSTRACT: u32 = 0x0400;
const ACC_SYNTHETIC: u32 = 0x1000;
const ACC_ENUM: u32 = 0x4000;
/// The keywords of a class's access flags, in `writeAccess`'s order. A class's `0x0020` is
/// `ACC_SUPER`, which no source keyword writes.
const CLASS_KEYWORDS: &[(u32, &str)] = &[
    (ACC_PRIVATE, "private"),
    (ACC_PROTECTED, "protected"),
    (ACC_PUBLIC, "public"),
    (ACC_STATIC, "static"),
    (ACC_FINAL, "final"),
    (ACC_ABSTRACT, "abstract"),
];
/// The keywords of a field's access flags, in `writeAccess`'s order.
const FIELD_KEYWORDS: &[(u32, &str)] = &[
    (ACC_PRIVATE, "private"),
    (ACC_PROTECTED, "protected"),
    (ACC_PUBLIC, "public"),
    (ACC_STATIC, "static"),
    (ACC_FINAL, "final"),
    (ACC_TRANSIENT, "transient"),
    (ACC_VOLATILE, "volatile"),
];
/// The keywords of a method's access flags, in `writeAccess`'s order. A method's `0x0040` is
/// `ACC_BRIDGE` and its `0x0080` `ACC_VARARGS`, which no keyword writes.
const METHOD_KEYWORDS: &[(u32, &str)] = &[
    (ACC_PRIVATE, "private"),
    (ACC_PROTECTED, "protected"),
    (ACC_PUBLIC, "public"),
    (ACC_STATIC, "static"),
    (ACC_FINAL, "final"),
    (ACC_ABSTRACT, "abstract"),
    (ACC_SYNCHRONIZED, "synchronized"),
    (ACC_NATIVE, "native"),
];

const UTF8: u8 = 1;
const INTEGER: u8 = 3;
const FLOAT: u8 = 4;
const LONG: u8 = 5;
const DOUBLE: u8 = 6;
const CLASS: u8 = 7;
const STRING: u8 = 8;
const NAME_AND_TYPE: u8 = 12;

/// Why the port does not read a class file.
#[derive(Debug)]
enum Unread {
    /// Its version is newer than ASM reads, or its constant pool has an entry of a kind ASM
    /// does not know: `ClassReader` throws before the document starts.
    Thrown(String),
    /// A structure ASM may read part of before it throws, or reads otherwise than the port.
    Malformed(&'static str),
}

/// A type, as ASM's `Type` gives its class name.
#[derive(Debug, Clone, PartialEq, Eq)]
enum JavaType {
    Primitive(&'static str),
    /// An object type, by its internal name.
    Object(String),
    Array(Box<JavaType>, usize),
}

impl JavaType {
    /// `Type.getClassName`.
    fn class_name(&self) -> String {
        match self {
            JavaType::Primitive(name) => (*name).to_owned(),
            JavaType::Object(internal) => internal.replace('/', "."),
            JavaType::Array(element, dimensions) => {
                format!("{}{}", element.class_name(), "[]".repeat(*dimensions))
            }
        }
    }

    /// `Type.getObjectType`: an array's descriptor or an object's internal name.
    fn of_internal_name(name: &str) -> Result<Self, Unread> {
        if name.starts_with('[') {
            JavaType::of_descriptor(name)
        } else {
            Ok(JavaType::Object(name.to_owned()))
        }
    }

    /// `Type.getType` of a field descriptor, which must be one whole type.
    fn of_descriptor(descriptor: &str) -> Result<Self, Unread> {
        let mut chars = descriptor.chars().peekable();
        let parsed = parse_type(&mut chars, false)?;
        match chars.next() {
            None => Ok(parsed),
            Some(_) => Err(Unread::Malformed("a descriptor with more than one type")),
        }
    }
}

/// One type of a descriptor, `void` only where `void_allowed`.
fn parse_type(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    void_allowed: bool,
) -> Result<JavaType, Unread> {
    let invalid = || Unread::Malformed("an invalid type descriptor");
    let mut dimensions = 0;
    while chars.peek() == Some(&'[') {
        chars.next();
        dimensions += 1;
    }
    let element = match chars.next().ok_or_else(invalid)? {
        'V' if void_allowed && dimensions == 0 => JavaType::Primitive("void"),
        'Z' => JavaType::Primitive("boolean"),
        'C' => JavaType::Primitive("char"),
        'B' => JavaType::Primitive("byte"),
        'S' => JavaType::Primitive("short"),
        'I' => JavaType::Primitive("int"),
        'F' => JavaType::Primitive("float"),
        'J' => JavaType::Primitive("long"),
        'D' => JavaType::Primitive("double"),
        'L' => {
            let mut internal = String::new();
            loop {
                match chars.next().ok_or_else(invalid)? {
                    ';' => break,
                    character => internal.push(character),
                }
            }
            if internal.is_empty() {
                return Err(invalid());
            }
            JavaType::Object(internal)
        }
        _ => return Err(invalid()),
    };
    Ok(if dimensions == 0 {
        element
    } else {
        JavaType::Array(Box::new(element), dimensions)
    })
}

/// `Type.getArgumentTypes` and `Type.getReturnType` of a method descriptor.
fn method_types(descriptor: &str) -> Result<(Vec<JavaType>, JavaType), Unread> {
    let mut chars = descriptor.chars().peekable();
    if chars.next() != Some('(') {
        return Err(Unread::Malformed(
            "a method descriptor without its arguments",
        ));
    }
    let mut arguments = Vec::new();
    while chars.peek() != Some(&')') {
        arguments.push(parse_type(&mut chars, false)?);
    }
    chars.next();
    let returned = parse_type(&mut chars, true)?;
    if chars.next().is_some() {
        return Err(Unread::Malformed(
            "a method descriptor with more than one return type",
        ));
    }
    Ok((arguments, returned))
}

/// The class file's bytes, read as ASM's `ClassReader` reads them, every read within them.
struct Reader<'a> {
    bytes: &'a [u8],
    /// The offset of each constant pool entry's tag, by its index.
    entries: Vec<Option<usize>>,
}

const TRUNCATED: Unread = Unread::Malformed("a structure past the end of the class file");

impl<'a> Reader<'a> {
    fn u8(&self, at: usize) -> Result<u8, Unread> {
        self.bytes.get(at).copied().ok_or(TRUNCATED)
    }

    fn u16(&self, at: usize) -> Result<u16, Unread> {
        let bytes = self.bytes.get(at..at + 2).ok_or(TRUNCATED)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&self, at: usize) -> Result<u32, Unread> {
        let bytes = self.bytes.get(at..at + 4).ok_or(TRUNCATED)?;
        Ok(u32::from_be_bytes(bytes.try_into().expect("four bytes")))
    }

    fn u64(&self, at: usize) -> Result<u64, Unread> {
        let bytes = self.bytes.get(at..at + 8).ok_or(TRUNCATED)?;
        Ok(u64::from_be_bytes(bytes.try_into().expect("eight bytes")))
    }

    /// The constant pool entry at `index`, which must be of the kind `tag`: where its data
    /// starts.
    fn entry(&self, index: u16, tag: u8) -> Result<usize, Unread> {
        let at = self
            .entries
            .get(usize::from(index))
            .copied()
            .flatten()
            .ok_or(Unread::Malformed("a reference to no constant pool entry"))?;
        if self.u8(at)? != tag {
            return Err(Unread::Malformed(
                "a reference to a constant pool entry of another kind",
            ));
        }
        Ok(at + 1)
    }

    /// `readUtf` of the `CONSTANT_Utf8` entry at `index`: modified UTF-8 decoded as ASM decodes
    /// it, a lone surrogate written as U+FFFD, as Tika's metadata and handler write it.
    fn utf8(&self, index: u16) -> Result<String, Unread> {
        let at = self.entry(index, UTF8)?;
        let length = usize::from(self.u16(at)?);
        let bytes = self.bytes.get(at + 2..at + 2 + length).ok_or(TRUNCATED)?;
        let mut units = Vec::with_capacity(length);
        let mut rest = bytes.iter().copied();
        while let Some(byte) = rest.next() {
            let mut next = || {
                rest.next()
                    .map(|byte| u16::from(byte & 0x3F))
                    .ok_or(Unread::Malformed("a UTF-8 constant cut inside a character"))
            };
            let unit = if byte & 0x80 == 0 {
                u16::from(byte & 0x7F)
            } else if byte & 0xE0 == 0xC0 {
                (u16::from(byte & 0x1F) << 6) + next()?
            } else {
                let high = next()?;
                (u16::from(byte & 0x0F) << 12) + (high << 6) + next()?
            };
            units.push(unit);
        }
        Ok(String::from_utf16_lossy(&units))
    }

    /// The internal name of the `CONSTANT_Class` entry at `index`.
    fn class(&self, index: u16) -> Result<String, Unread> {
        let at = self.entry(index, CLASS)?;
        self.utf8(self.u16(at)?)
    }

    /// `readConst(...).toString()` of a field's constant value.
    fn constant(&self, index: u16) -> Result<String, Unread> {
        let at = self
            .entries
            .get(usize::from(index))
            .copied()
            .flatten()
            .ok_or(Unread::Malformed("a reference to no constant pool entry"))?;
        Ok(match self.u8(at)? {
            INTEGER => (self.u32(at + 1)? as i32).to_string(),
            FLOAT => java_float_to_string(f32::from_bits(self.u32(at + 1)?)),
            LONG => (self.u64(at + 1)? as i64).to_string(),
            DOUBLE => java_double_to_string(f64::from_bits(self.u64(at + 1)?)),
            STRING => self.utf8(self.u16(at + 1)?)?,
            _ => {
                return Err(Unread::Malformed(
                    "a constant value that is no number or string",
                ))
            }
        })
    }
}

/// An attribute of a class, field or method: its name, and its data and where that starts.
struct Attribute<'a> {
    name: String,
    at: usize,
    data: &'a [u8],
}

/// Reads the attributes at `at`: them, and where they end.
fn attributes<'a>(reader: &Reader<'a>, at: usize) -> Result<(Vec<Attribute<'a>>, usize), Unread> {
    let count = reader.u16(at)?;
    let mut at = at + 2;
    let mut read = Vec::with_capacity(usize::from(count));
    for _ in 0..count {
        let name = reader.utf8(reader.u16(at)?)?;
        let length = reader.u32(at + 2)? as usize;
        let data = reader.bytes.get(at + 6..at + 6 + length).ok_or(TRUNCATED)?;
        read.push(Attribute {
            name,
            at: at + 6,
            data,
        });
        at += 6 + length;
    }
    Ok((read, at))
}

/// The access flags, with `ACC_SYNTHETIC` where a `Synthetic` attribute says so, as ASM adds it.
fn with_attributes(access: u32, attributes: &[Attribute<'_>]) -> u32 {
    if attributes
        .iter()
        .any(|attribute| attribute.name == "Synthetic")
    {
        access | ACC_SYNTHETIC
    } else {
        access
    }
}

/// The two bytes at the start of an attribute's data.
fn first_u16(attribute: &Attribute<'_>) -> Result<u16, Unread> {
    let bytes = attribute.data.get(..2).ok_or(TRUNCATED)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

struct Field {
    access: u32,
    name: String,
    kind: JavaType,
    value: Option<String>,
}

struct Method {
    access: u32,
    name: String,
    arguments: Vec<JavaType>,
    returned: JavaType,
    exceptions: Vec<JavaType>,
}

/// What `XHTMLClassVisitor` is told of a class.
struct Class {
    access: u32,
    kind: JavaType,
    super_type: Option<JavaType>,
    interfaces: Vec<JavaType>,
    fields: Vec<Field>,
    methods: Vec<Method>,
}

/// The signature a `Signature` attribute names, which ASM reads, must be a UTF-8 constant.
fn check_signature(reader: &Reader<'_>, attributes: &[Attribute<'_>]) -> Result<(), Unread> {
    for attribute in attributes
        .iter()
        .filter(|attribute| attribute.name == "Signature")
    {
        reader.utf8(first_u16(attribute)?)?;
    }
    Ok(())
}

/// The class names a list of `count` constant pool indexes at the start of `data` names, each
/// a class, or none where `optional` allows a zero.
fn check_classes(
    reader: &Reader<'_>,
    data: &[u8],
    stride: usize,
    optional: bool,
) -> Result<(), Unread> {
    let count = usize::from(u16::from_be_bytes([
        *data.first().ok_or(TRUNCATED)?,
        *data.get(1).ok_or(TRUNCATED)?,
    ]));
    for number in 0..count {
        let at = 2 + number * stride;
        let entry = data.get(at..at + 2).ok_or(TRUNCATED)?;
        match u16::from_be_bytes([entry[0], entry[1]]) {
            0 if optional => {}
            index => {
                reader.class(index)?;
            }
        }
    }
    Ok(())
}

/// What `accept` reads of a class attribute: the constants it names, and the annotations it
/// walks past, element value by element value, within the class file.
fn check_class_attribute(reader: &Reader<'_>, attribute: &Attribute<'_>) -> Result<(), Unread> {
    let data = attribute.data;
    match attribute.name.as_str() {
        "SourceFile" | "Signature" => {
            reader.utf8(first_u16(attribute)?)?;
        }
        "NestHost" => {
            reader.class(first_u16(attribute)?)?;
        }
        "NestMembers" | "PermittedSubclasses" => check_classes(reader, data, 2, false)?,
        "InnerClasses" => {
            check_classes(reader, data, 8, true)?;
            let count = usize::from(first_u16(attribute)?);
            for number in 0..count {
                let at = 2 + number * 8;
                let entry = data.get(at..at + 8).ok_or(TRUNCATED)?;
                for (offset, utf8) in [(2, false), (4, true)] {
                    match u16::from_be_bytes([entry[offset], entry[offset + 1]]) {
                        0 => {}
                        index if utf8 => {
                            reader.utf8(index)?;
                        }
                        index => {
                            reader.class(index)?;
                        }
                    }
                }
            }
        }
        "EnclosingMethod" => {
            reader.class(first_u16(attribute)?)?;
            let method = data.get(2..4).ok_or(TRUNCATED)?;
            match u16::from_be_bytes([method[0], method[1]]) {
                0 => {}
                method => {
                    let at = reader.entry(method, NAME_AND_TYPE)?;
                    reader.utf8(reader.u16(at)?)?;
                    reader.utf8(reader.u16(at + 2)?)?;
                }
            }
        }
        "RuntimeVisibleAnnotations" | "RuntimeInvisibleAnnotations" => {
            let count = reader.u16(attribute.at)?;
            let mut at = attribute.at + 2;
            for _ in 0..count {
                at = skip_element_values(reader, at + 2, true)?;
            }
        }
        "Deprecated" | "Synthetic" | "SourceDebugExtension" | "BootstrapMethods" => {}
        "RuntimeVisibleTypeAnnotations"
        | "RuntimeInvisibleTypeAnnotations"
        | "Record"
        | "Module"
        | "ModulePackages"
        | "ModuleMainClass" => {
            return Err(Unread::Malformed(
                "type annotations, record components or a module, which ASM walks otherwise",
            ))
        }
        _ => {}
    }
    Ok(())
}

/// `readElementValues` with no visitor: where the element values at `at` end, each named
/// where `named`, as ASM skips them, whose every read must be within the class file.
fn skip_element_values(reader: &Reader<'_>, at: usize, named: bool) -> Result<usize, Unread> {
    let count = reader.u16(at)?;
    let mut at = at + 2;
    for _ in 0..count {
        if named {
            at += 2;
        }
        at = match reader.u8(at)? {
            b'e' => at + 5,
            b'@' => skip_element_values(reader, at + 3, true)?,
            b'[' => skip_element_values(reader, at + 1, false)?,
            _ => at + 3,
        };
    }
    Ok(at)
}

/// `new ClassReader(stream)`, then what `accept` hands the visitor with `SKIP_CODE`.
fn read(bytes: &[u8]) -> Result<Class, Unread> {
    let thrown = |what: &str| Unread::Thrown(what.to_owned());
    let header = bytes
        .get(..10)
        .ok_or_else(|| thrown("ArrayIndexOutOfBoundsException: the class file is too short"))?;
    let version = u16::from_be_bytes([header[6], header[7]]);
    if version > NEWEST_VERSION {
        return Err(Unread::Thrown(format!(
            "IllegalArgumentException: Unsupported class file major version {version}"
        )));
    }
    let count = usize::from(u16::from_be_bytes([header[8], header[9]]));
    let mut entries = vec![None; count];
    let mut at = 10;
    let mut index = 1;
    while index < count {
        entries[index] = Some(at);
        let tag = *bytes.get(at).ok_or_else(|| {
            thrown("ArrayIndexOutOfBoundsException: the constant pool ends past the class file")
        })?;
        let size = match tag {
            9 | 10 | 11 | INTEGER | FLOAT | NAME_AND_TYPE | 17 | 18 => 5,
            LONG | DOUBLE => {
                index += 1;
                9
            }
            UTF8 => {
                let length = bytes.get(at + 1..at + 3).ok_or_else(|| {
                    thrown("ArrayIndexOutOfBoundsException: the constant pool ends past the class file")
                })?;
                3 + usize::from(u16::from_be_bytes([length[0], length[1]]))
            }
            15 => 4,
            CLASS | STRING | 16 | 19 | 20 => 3,
            _ => {
                return Err(thrown(
                    "IllegalArgumentException: a constant pool tag ASM does not know",
                ))
            }
        };
        index += 1;
        at += size;
    }
    let reader = Reader { bytes, entries };
    let access = u32::from(reader.u16(at)?);
    let name = reader.class(reader.u16(at + 2)?)?;
    let super_index = reader.u16(at + 4)?;
    let super_type = match super_index {
        0 => None,
        index => Some(JavaType::of_internal_name(&reader.class(index)?)?),
    };
    let interface_count = reader.u16(at + 6)?;
    let mut interfaces = Vec::with_capacity(usize::from(interface_count));
    at += 8;
    for _ in 0..interface_count {
        interfaces.push(JavaType::of_internal_name(&reader.class(reader.u16(at)?)?)?);
        at += 2;
    }
    let mut fields = Vec::new();
    let field_count = reader.u16(at)?;
    at += 2;
    for _ in 0..field_count {
        let access = u32::from(reader.u16(at)?);
        let name = reader.utf8(reader.u16(at + 2)?)?;
        let descriptor = reader.utf8(reader.u16(at + 4)?)?;
        let (attributes, end) = attributes(&reader, at + 6)?;
        check_signature(&reader, &attributes)?;
        let value = match attributes
            .iter()
            .rev()
            .find(|attribute| attribute.name == "ConstantValue")
        {
            Some(attribute) => match first_u16(attribute)? {
                0 => None,
                index => Some(reader.constant(index)?),
            },
            None => None,
        };
        fields.push(Field {
            access: with_attributes(access, &attributes),
            name,
            kind: JavaType::of_descriptor(&descriptor)?,
            value,
        });
        at = end;
    }
    let mut methods = Vec::new();
    let method_count = reader.u16(at)?;
    at += 2;
    for _ in 0..method_count {
        let access = u32::from(reader.u16(at)?);
        let name = reader.utf8(reader.u16(at + 2)?)?;
        let descriptor = reader.utf8(reader.u16(at + 4)?)?;
        let (attributes, end) = attributes(&reader, at + 6)?;
        check_signature(&reader, &attributes)?;
        let mut exceptions = Vec::new();
        if let Some(attribute) = attributes
            .iter()
            .rev()
            .find(|attribute| attribute.name == "Exceptions")
        {
            let count = first_u16(attribute)?;
            for number in 0..usize::from(count) {
                let entry = attribute
                    .data
                    .get(2 + 2 * number..4 + 2 * number)
                    .ok_or(TRUNCATED)?;
                let class = reader.class(u16::from_be_bytes([entry[0], entry[1]]))?;
                exceptions.push(JavaType::of_internal_name(&class)?);
            }
        }
        let (arguments, returned) = method_types(&descriptor)?;
        methods.push(Method {
            access: with_attributes(access, &attributes),
            name,
            arguments,
            returned,
            exceptions,
        });
        at = end;
    }
    let (class_attributes, _) = attributes(&reader, at)?;
    for attribute in &class_attributes {
        check_class_attribute(&reader, attribute)?;
    }
    Ok(Class {
        access: with_attributes(access, &class_attributes),
        kind: JavaType::of_internal_name(&name)?,
        super_type,
        interfaces,
        fields,
        methods,
    })
}

/// `XHTMLClassVisitor`'s writing of a class into the `pre` element.
struct Writer<'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    /// What a type's class name starts with where it is in the class's package, none for a
    /// class in the unnamed package.
    package_prefix: Option<String>,
}

impl Writer<'_, '_> {
    fn text(&mut self, text: &str) -> Result<(), Failure> {
        Ok(self.xhtml.characters(text)?)
    }

    fn span(&mut self, class: &str, text: &str) -> Result<(), Failure> {
        self.xhtml.start_element_with("span", &[("class", class)])?;
        self.text(text)?;
        Ok(self.xhtml.end_element("span")?)
    }

    fn keyword(&mut self, keyword: &str) -> Result<(), Failure> {
        self.span("java-keyword", keyword)
    }

    fn access(&mut self, access: u32, keywords: &[(u32, &str)]) -> Result<(), Failure> {
        for &(flag, keyword) in keywords {
            if access & flag != 0 {
                self.keyword(keyword)?;
                self.text(" ")?;
            }
        }
        Ok(())
    }

    /// `writeType`: the class name, without the class's package or `java.lang`.
    fn kind(&mut self, kind: &JavaType) -> Result<(), Failure> {
        let name = kind.class_name();
        let shown = self
            .package_prefix
            .as_deref()
            .and_then(|prefix| name.strip_prefix(prefix))
            .or_else(|| name.strip_prefix("java.lang."))
            .unwrap_or(&name)
            .to_owned();
        self.text(&shown)
    }

    fn interfaces(&mut self, keyword: &str, interfaces: &[JavaType]) -> Result<(), Failure> {
        if interfaces.is_empty() {
            return Ok(());
        }
        self.keyword(keyword)?;
        let mut separator = " ";
        for interface in interfaces {
            self.text(separator)?;
            self.kind(interface)?;
            separator = ", ";
        }
        self.text(" ")
    }

    fn declaration(&mut self, class: &Class, package: Option<&str>) -> Result<(), Failure> {
        self.xhtml.start_element("pre")?;
        if let Some(package) = package {
            self.keyword("package")?;
            self.text(&format!(" {package};\n"))?;
        }
        self.access(class.access, CLASS_KEYWORDS)?;
        if class.access & ACC_INTERFACE != 0 {
            self.keyword("interface")?;
            self.text(" ")?;
            self.kind(&class.kind)?;
            self.text(" ")?;
            self.interfaces("extends", &class.interfaces)?;
        } else if class.access & ACC_ENUM != 0 {
            self.keyword("enum")?;
            self.text(" ")?;
            self.kind(&class.kind)?;
            self.text(" ")?;
        } else {
            self.keyword("class")?;
            self.text(" ")?;
            self.kind(&class.kind)?;
            self.text(" ")?;
            if let Some(super_type) = &class.super_type {
                if super_type.class_name() != "java.lang.Object" {
                    self.keyword("extends")?;
                    self.text(" ")?;
                    self.kind(super_type)?;
                    self.text(" ")?;
                }
            }
            self.interfaces("implements", &class.interfaces)?;
        }
        self.text("{\n")
    }

    fn field(&mut self, field: &Field) -> Result<(), Failure> {
        if field.access & ACC_SYNTHETIC != 0 {
            return Ok(());
        }
        self.text("    ")?;
        self.access(field.access, FIELD_KEYWORDS)?;
        self.kind(&field.kind)?;
        self.text(" ")?;
        self.span("java-identifier", &field.name)?;
        if let (true, Some(value)) = (field.access & ACC_STATIC != 0, &field.value) {
            self.text(" = ")?;
            self.text(value)?;
        }
        self.text(";")?;
        self.text("\n")
    }

    fn method(&mut self, method: &Method, class: &JavaType) -> Result<(), Failure> {
        if method.access & ACC_SYNTHETIC != 0 {
            return Ok(());
        }
        self.text("    ")?;
        self.access(method.access, METHOD_KEYWORDS)?;
        self.kind(&method.returned)?;
        self.text(" ")?;
        if method.name == "<init>" {
            self.kind(class)?;
        } else {
            self.span("java-identifier", &method.name)?;
        }
        self.text("(")?;
        let mut separator = "";
        for argument in &method.arguments {
            self.text(separator)?;
            self.kind(argument)?;
            separator = ", ";
        }
        self.text(")")?;
        if !method.exceptions.is_empty() {
            self.text(" ")?;
            self.keyword("throws")?;
            let mut separator = " ";
            for exception in &method.exceptions {
                self.text(separator)?;
                self.kind(exception)?;
                separator = ", ";
            }
        }
        self.text(";")?;
        self.text("\n")
    }
}

/// Tika's `ClassParser`.
pub struct ClassParser;

impl Parser for ClassParser {
    fn name(&self) -> &'static str {
        "extractous::ClassParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.asm.ClassParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/java-vm"]
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _: &TikaMetadata,
        _: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let class = read(input).map_err(|unread| match unread {
            Unread::Thrown(cause) => Decline::Fails(Error::ParseError(format!(
                "Failed to parse a Java class: {cause}"
            ))),
            Unread::Malformed(what) => Unported::new(format!(
                "a class file with {what}, which ASM reads up to where it throws"
            ))
            .into(),
        })?;
        let full_name = class.kind.class_name();
        let (package, simple) = match full_name.rfind('.') {
            Some(dot) => (
                Some(full_name[..dot].to_owned()),
                full_name[dot + 1..].to_owned(),
            ),
            None => (None, full_name.clone()),
        };
        Ok(Box::new(move |xhtml| {
            xhtml.metadata_mut().set("dc:title", simple.as_str());
            xhtml
                .metadata_mut()
                .set("tk:resource-name", format!("{simple}.class"));
            let package_prefix = package.as_deref().map(|package| format!("{package}."));
            let mut writer = Writer {
                xhtml,
                package_prefix,
            };
            writer.declaration(&class, package.as_deref())?;
            for field in &class.fields {
                writer.field(field)?;
            }
            for method in &class.methods {
                writer.method(method, &class.kind)?;
            }
            writer.text("}\n")?;
            Ok(writer.xhtml.end_element("pre")?)
        }))
    }
}

/// For a parity comparison (#132): a text without the keywords Tika's path writes for access
/// flags that no keyword stands for where they are set, `synchronized` for a class's
/// `ACC_SUPER` and `transient` for a method's `ACC_VARARGS`, which the native parser leaves out.
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub fn without_misread_access(text: &mut String) {
    let without = |line: &str, word: &str| {
        let span = format!("<span class=\"java-keyword\">{word}</span> ");
        if line.contains(&span) {
            line.replacen(&span, "", 1)
        } else {
            line.replacen(&format!("{word} "), "", 1)
        }
    };
    let mut normalised = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let member = line.starts_with("    ");
        if !member && line.contains("synchronized") {
            normalised.push_str(&without(line, "synchronized"));
        } else if member && line.contains('(') {
            normalised.push_str(&without(line, "transient"));
        } else {
            normalised.push_str(line);
        }
    }
    *text = normalised;
}

/// For a parity comparison whose departure only a normalisation of both texts takes out: the
/// native result as it is.
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub fn as_written(
    _: &mut String,
    _: &mut crate::formats::parity::ComparedMetadata,
    _: &crate::formats::parity::ComparedMetadata,
) {
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "tika")]
    use std::path::{Path, PathBuf};

    use super::*;
    #[cfg(feature = "tika")]
    use crate::formats::parity::{assert_same_as_tika_normalising, conformance_corpus, documents};

    /// A class file of the class `access` `name` in the unnamed package, extending `Object`,
    /// with the fields and methods given by their access, name and descriptor.
    fn class_file(
        access: u16,
        name: &str,
        fields: &[(u16, &str, &str)],
        methods: &[(u16, &str, &str)],
    ) -> Vec<u8> {
        fn utf8(pool: &mut Vec<Vec<u8>>, text: &str) -> u16 {
            let mut entry = vec![UTF8];
            entry.extend(u16::try_from(text.len()).unwrap().to_be_bytes());
            entry.extend(text.as_bytes());
            pool.push(entry);
            u16::try_from(pool.len()).unwrap()
        }
        fn class(pool: &mut Vec<Vec<u8>>, name: &str) -> u16 {
            let name = utf8(pool, name);
            pool.push([&[CLASS][..], &name.to_be_bytes()].concat());
            u16::try_from(pool.len()).unwrap()
        }
        fn members(pool: &mut Vec<Vec<u8>>, list: &[(u16, &str, &str)]) -> Vec<u8> {
            let mut bytes = u16::try_from(list.len()).unwrap().to_be_bytes().to_vec();
            for (access, name, descriptor) in list {
                bytes.extend(access.to_be_bytes());
                bytes.extend(utf8(pool, name).to_be_bytes());
                bytes.extend(utf8(pool, descriptor).to_be_bytes());
                bytes.extend(0u16.to_be_bytes());
            }
            bytes
        }
        let mut pool = Vec::new();
        let this = class(&mut pool, name);
        let object = class(&mut pool, "java/lang/Object");
        let fields = members(&mut pool, fields);
        let methods = members(&mut pool, methods);
        let mut bytes = vec![0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 52];
        bytes.extend(u16::try_from(pool.len() + 1).unwrap().to_be_bytes());
        bytes.extend(pool.concat());
        bytes.extend(access.to_be_bytes());
        bytes.extend(this.to_be_bytes());
        bytes.extend(object.to_be_bytes());
        bytes.extend(0u16.to_be_bytes());
        bytes.extend(fields);
        bytes.extend(methods);
        bytes.extend(0u16.to_be_bytes());
        bytes
    }

    #[test]
    fn writes_the_keywords_of_the_flags_set_and_types_by_their_names() {
        let bytes = class_file(
            0x0021,
            "Wave",
            &[(0x0082, "height", "Lnull/Crest;")],
            &[(0x0081, "sum", "([I)I"), (0x0021, "<init>", "()V")],
        );
        let (text, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(&bytes)
            .unwrap();
        assert_eq!(metadata["dc:title"], ["Wave"]);
        assert_eq!(
            text.trim(),
            "public class Wave {\n    private transient null.Crest height;\n    \
             public int sum(int[]);\n    public synchronized void Wave();\n}"
        );
    }

    #[test]
    fn names_types_as_asm_does() {
        assert_eq!(
            JavaType::of_descriptor("[[Ljava/util/List;")
                .unwrap()
                .class_name(),
            "java.util.List[][]"
        );
        assert_eq!(JavaType::of_descriptor("J").unwrap().class_name(), "long");
        let (arguments, returned) = method_types("(I[BLjava/lang/String;)V").unwrap();
        let names: Vec<String> = arguments.iter().map(JavaType::class_name).collect();
        assert_eq!(names, ["int", "byte[]", "java.lang.String"]);
        assert_eq!(returned.class_name(), "void");
        assert!(method_types("(V)V").is_err());
        assert!(JavaType::of_descriptor("Ljava/lang/String").is_err());
    }

    #[cfg(feature = "tika")]
    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_classes_as_tikas_path_does() {
        let (parsed, deferred) = documents(&ClassParser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 2, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika_normalising(
            &paths,
            ClassParser.tika_name(),
            as_written,
            without_misread_access,
        );
    }
}
