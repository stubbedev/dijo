//! Executables, as Tika's `ExecutableParser` reads the headers of PE, ELF and Mach-O files, and
//! universal Mach-O binaries, as its `UniversalExecutableParser` hands each architecture's
//! Mach-O file to the embedded document extractor.

use crate::config::ParserConfigs;
use crate::formats::embedded::Wrapping;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Parser, Prepared};
use crate::java::time::format_instant;

const PLATFORM: &str = "machine:platform";
const MACHINE_TYPE: &str = "machine:machineType";
const ENDIAN: &str = "machine:endian";
const ARCHITECTURE_BITS: &str = "machine:architectureBits";
const LITTLE: &str = "Little";
const BIG: &str = "Big";
/// `MAX_ARCHS_COUNT`.
const MAX_ARCHITECTURES: i32 = 1000;
/// `MAX_ARCH_SIZE`.
const MAX_ARCHITECTURE_SIZE: i64 = 500_000_000;

/// What a header gives, set in order, and the exception that ends the parse where one does.
#[derive(Debug, Default)]
struct Header {
    sets: Vec<(&'static str, String)>,
    thrown: Option<String>,
}

impl Header {
    fn set(&mut self, name: &'static str, value: impl Into<String>) {
        self.sets.push((name, value.into()));
    }
}

/// The input read as Tika's parsers read the `TikaInputStream`: an `EOFException` or a
/// `BufferUnderrunException` where it ends short of what they read.
struct Input<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Input<'a> {
    /// `IOUtils.readFully`.
    fn read_fully(&mut self, length: usize) -> Result<&'a [u8], String> {
        let available = self.bytes.len() - self.at;
        if available < length {
            self.at = self.bytes.len();
            return Err(format!(
                "EOFException: Length to read: {length} actual: {available}"
            ));
        }
        let read = &self.bytes[self.at..self.at + length];
        self.at += length;
        Ok(read)
    }

    /// `InputStream.read()`: a byte, or -1 at the end.
    fn read(&mut self) -> i32 {
        match self.bytes.get(self.at) {
            Some(&byte) => {
                self.at += 1;
                i32::from(byte)
            }
            None => -1,
        }
    }

    /// `EndianUtils`' stream readers: the bytes of a number, or a `BufferUnderrunException`.
    fn number<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let bytes = self.bytes.get(self.at..self.at + N).ok_or_else(|| {
            self.at = self.bytes.len();
            "BufferUnderrunException".to_owned()
        })?;
        self.at += N;
        Ok(bytes.try_into().expect("N bytes"))
    }

    fn u16(&mut self, little: bool) -> Result<u16, String> {
        let bytes = self.number::<2>()?;
        Ok(if little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    }

    fn i32(&mut self, little: bool) -> Result<i32, String> {
        let bytes = self.number::<4>()?;
        Ok(if little {
            i32::from_le_bytes(bytes)
        } else {
            i32::from_be_bytes(bytes)
        })
    }

    fn i64(&mut self, little: bool) -> Result<i64, String> {
        let bytes = self.number::<8>()?;
        Ok(if little {
            i64::from_le_bytes(bytes)
        } else {
            i64::from_be_bytes(bytes)
        })
    }

    /// `TikaInputStream.skip`: as much as is left.
    fn skip(&mut self, count: usize) {
        self.at = self.at.saturating_add(count).min(self.bytes.len());
    }

    /// `IOUtils.skipFully`.
    fn skip_fully(&mut self, count: i64) -> Result<(), String> {
        let count = usize::try_from(count).map_err(|_| {
            format!("IllegalArgumentException: Skip count must be non-negative, actual: {count}")
        })?;
        self.read_fully(count).map(|_| ())
    }
}

pub struct ExecutableParser;

impl Parser for ExecutableParser {
    fn name(&self) -> &'static str {
        "extractous::ExecutableParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.executable.ExecutableParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/x-msdownload",
            "application/x-dosexec",
            "application/x-elf",
            "application/x-object",
            "application/x-executable",
            "application/x-sharedlib",
            "application/x-coredump",
            "application/x-mach-o",
            "application/x-mach-o-object",
            "application/x-mach-o-executable",
            "application/x-mach-o-fvmlib",
            "application/x-mach-o-core",
            "application/x-mach-o-preload",
            "application/x-mach-o-dylib",
            "application/x-mach-o-dylinker",
            "application/x-mach-o-bundle",
            "application/x-mach-o-dylib-stub",
            "application/x-mach-o-dsym",
            "application/x-mach-o-kext-bundle",
        ]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let header = header(input);
        Ok(Box::new(move |xhtml| {
            let metadata = xhtml.metadata_mut();
            for (name, value) in &header.sets {
                metadata.set(name, value.as_str());
            }
            match &header.thrown {
                Some(thrown) => Err(Failure::Malformed(thrown.clone())),
                None => Ok(()),
            }
        }))
    }
}

/// `ExecutableParser.parse`: the header the first four bytes announce.
fn header(input: &[u8]) -> Header {
    let mut header = Header::default();
    let mut stream = Input {
        bytes: input,
        at: 0,
    };
    let read = stream.read_fully(4).and_then(|first| {
        let first: [u8; 4] = first.try_into().expect("four bytes");
        match first {
            [b'M', b'Z', ..] => pe(&mut stream, &mut header),
            [0x7F, b'E', b'L', b'F'] => elf(&mut stream, &mut header),
            [0xCF | 0xCE, 0xFA, 0xED, 0xFE] => mach_o(&mut stream, true, &mut header),
            [0xFE, 0xED, 0xFA, 0xCF | 0xCE] => mach_o(&mut stream, false, &mut header),
            _ => Ok(()),
        }
    });
    header.thrown = read.err();
    header
}

/// `parsePE`: the COFF header the MS-DOS header points to, if it is there. An offset into the
/// MS-DOS header itself, which Tika's reads past it, points to none (#118).
fn pe(stream: &mut Input<'_>, header: &mut Header) -> Result<(), String> {
    header.set("Content-Type", "application/x-msdownload");
    header.set(PLATFORM, "Windows");
    stream.read_fully(0x3c - 4)?;
    let offset = stream.i32(true)?;
    let Some(gap) = usize::try_from(offset)
        .ok()
        .filter(|&offset| offset <= 4096)
        .and_then(|offset| offset.checked_sub(0x40))
    else {
        return Ok(());
    };
    stream.skip(gap);
    let coff = stream.read_fully(24)?;
    if &coff[..4] != b"PE\0\0" {
        return Ok(());
    }
    let machine = u16::from_le_bytes([coff[4], coff[5]]);
    let created = i32::from_le_bytes(coff[8..12].try_into().expect("four bytes"));
    header.set("dcterms:created", format_instant(i64::from(created) * 1000));
    let (machine_type, endian, bits) = match machine {
        0x14c => ("x86-32", Some(LITTLE), Some("32")),
        0x8664 => ("x86-64", Some(LITTLE), Some("64")),
        0x200 => ("IA-64", Some(LITTLE), Some("64")),
        0x184 => ("Alpha", Some(LITTLE), Some("32")),
        0x284 => ("Alpha", Some(LITTLE), Some("64")),
        0x1c0 | 0x1c4 => ("ARM", Some(LITTLE), Some("32")),
        0x268 => ("Motorola-68000", Some(BIG), Some("32")),
        0x266 | 0x366 | 0x466 => ("MIPS", Some(BIG), Some("16")),
        0x162 | 0x166 | 0x168 | 0x169 => ("MIPS", Some(LITTLE), Some("16")),
        0x1f0 | 0x1f1 => ("PPC", Some(LITTLE), Some("32")),
        0x1a2 | 0x1a3 | 0x1a8 => ("SH3", Some(BIG), Some("32")),
        0x1a6 => ("SH4", Some(BIG), Some("32")),
        0x9041 => ("M32R", Some(BIG), Some("32")),
        0xebc => ("EFI", None, None),
        _ => ("Unknown", None, None),
    };
    header.set(MACHINE_TYPE, machine_type);
    if let Some(endian) = endian {
        header.set(ENDIAN, endian);
    }
    if let Some(bits) = bits {
        header.set(ARCHITECTURE_BITS, bits);
    }
    Ok(())
}

/// `parseELF`: the class, data encoding, OS ABI, type and machine of the ELF header.
fn elf(stream: &mut Input<'_>, header: &mut Header) -> Result<(), String> {
    match stream.read() {
        1 => header.set(ARCHITECTURE_BITS, "32"),
        2 => header.set(ARCHITECTURE_BITS, "64"),
        _ => {}
    }
    let endian = stream.read();
    match endian {
        1 => header.set(ENDIAN, LITTLE),
        2 => header.set(ENDIAN, BIG),
        _ => {}
    }
    stream.read();
    let os = stream.read();
    let os_version = stream.read();
    if os > 0 || os_version > 0 {
        let platform = match os {
            0 => Some("System V"),
            1 => Some("HP-UX"),
            2 => Some("NetBSD"),
            3 => Some("Linux"),
            6 => Some("Solaris"),
            7 => Some("AIX"),
            8 => Some("IRIX"),
            9 | 12 => Some("FreeBSD"),
            10 => Some("Tru64"),
            64 | 97 => Some("ARM"),
            255 => Some("Embedded"),
            _ => None,
        };
        if let Some(platform) = platform {
            header.set(PLATFORM, platform);
        }
    }
    stream.read_fully(7)?;
    let little = endian == 1;
    header.set(
        "Content-Type",
        match stream.u16(little)? {
            1 => "application/x-object",
            2 => "application/x-executable",
            3 => "application/x-sharedlib",
            4 => "application/x-coredump",
            _ => "application/x-elf",
        },
    );
    let machine = match stream.u16(little)? {
        2 | 18 | 43 => Some("SPARC"),
        3 => Some("x86-32"),
        4 => Some("Motorola-68000"),
        5 => Some("Motorola-88000"),
        8 | 10 => Some("MIPS"),
        7 => Some("S370"),
        20 | 21 => Some("PPC"),
        22 => Some("S390"),
        40 => Some("ARM"),
        41 | 0x9026 => Some("Alpha"),
        50 => Some("IA-64"),
        62 => Some("x86-64"),
        75 => Some("Vax"),
        88 => Some("M32R"),
        _ => None,
    };
    if let Some(machine) = machine {
        header.set(MACHINE_TYPE, machine);
    }
    Ok(())
}

/// `parseMachO`: the CPU type and file type of the Mach-O header.
fn mach_o(stream: &mut Input<'_>, little: bool, header: &mut Header) -> Result<(), String> {
    header.set(ENDIAN, if little { LITTLE } else { BIG });
    let cpu_type = stream.i32(little)?;
    if cpu_type >> 24 == 1 {
        header.set(ARCHITECTURE_BITS, "64");
    }
    let machine = match cpu_type {
        1 => Some("Vax"),
        6 => Some("Motorola-68000"),
        7 => Some("x86-32"),
        0x0100_0007 => Some("x86-64"),
        8 => Some("MIPS"),
        12 | 0x0100_000C => Some("ARM"),
        13 => Some("Motorola-88000"),
        14 => Some("SPARC"),
        18 => Some("PPC"),
        _ => None,
    };
    if let Some(machine) = machine {
        header.set(MACHINE_TYPE, machine);
    }
    stream.i32(little)?;
    header.set(
        "Content-Type",
        match stream.i32(little)? {
            0x1 => "application/x-mach-o-object",
            0x2 => "application/x-mach-o-executable",
            0x3 => "application/x-mach-o-fvmlib",
            0x4 => "application/x-mach-o-core",
            0x5 => "application/x-mach-o-preload",
            0x6 => "application/x-mach-o-dylib",
            0x7 => "application/x-mach-o-dylinker",
            0x8 => "application/x-mach-o-bundle",
            0x9 => "application/x-mach-o-dylib-stub",
            0xa => "application/x-mach-o-dsym",
            0xb => "application/x-mach-o-kext-bundle",
            _ => "application/x-mach-o",
        },
    );
    Ok(())
}

pub struct UniversalExecutableParser;

impl Parser for UniversalExecutableParser {
    fn name(&self) -> &'static str {
        "extractous::UniversalExecutableParser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.executable.UniversalExecutableParser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "application/x-mach-o-universal",
            "application/x-java-jnilib",
        ]
    }

    fn footprint(&self) -> Footprint {
        Footprint::Bounded
    }

    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let (architectures, thrown) = architectures(input);
        Ok(Box::new(move |xhtml: &mut Xhtml| {
            for architecture in &architectures {
                let mut metadata = TikaMetadata::new();
                metadata.set("Content-Length", architecture.len().to_string());
                xhtml.parse_embedded(architecture, &metadata, Wrapping::PackageEntry)?;
            }
            match &thrown {
                Some(thrown) => Err(Failure::Malformed(thrown.clone())),
                None => Ok(()),
            }
        }))
    }
}

/// `UniversalExecutableParser.parse`: the Mach-O file of each architecture of the fat header,
/// in the order of their offsets, up to the exception that ends the parse where one does.
fn architectures(input: &[u8]) -> (Vec<&[u8]>, Option<String>) {
    let mut found = Vec::new();
    let thrown = read_architectures(input, &mut found).err();
    (found, thrown)
}

fn read_architectures<'a>(input: &'a [u8], found: &mut Vec<&'a [u8]>) -> Result<(), String> {
    let mut stream = Input {
        bytes: input,
        at: 0,
    };
    let first: [u8; 4] = stream.read_fully(4)?.try_into().expect("four bytes");
    let little = match first {
        [0xBF | 0xBE, 0xBA, 0xFE, 0xCA] => true,
        [0xCA, 0xFE, 0xBA, 0xBF | 0xBE] => false,
        _ => {
            return Err("UnsupportedFormatException: Not a universal executable file".to_owned());
        }
    };
    let wide = first[if little { 0 } else { 3 }] == 0xBF;
    let entry_size: i64 = if wide { 32 } else { 20 };
    let count = stream.i32(little)?;
    if count < 1 {
        return Err(format!(
            "TikaException: Invalid number of architectures: {count}"
        ));
    }
    if count > MAX_ARCHITECTURES {
        return Err(format!(
            "TikaException: Number of architectures={count} greater than max allowed={MAX_ARCHITECTURES}"
        ));
    }
    let entries_size = i64::from(count) * entry_size;
    let mut position: i64 = 8;
    let mut entries: Vec<(i64, i64)> = Vec::with_capacity(count as usize);
    for _ in 0..count {
        stream.skip_fully(8)?;
        let offset = if wide {
            stream.i64(little)?
        } else {
            i64::from(stream.i32(little)?)
        };
        if offset < 8 + entries_size {
            return Err(format!("TikaException: Invalid offset: {offset}"));
        }
        let size = if wide {
            stream.i64(little)?
        } else {
            i64::from(stream.i32(little)?)
        };
        if !(0..=MAX_ARCHITECTURE_SIZE).contains(&size) {
            return Err(format!(
                "TikaException: Arch size={size} must be > 0 and < {MAX_ARCHITECTURE_SIZE}"
            ));
        }
        entries.push((offset, size));
        stream.skip_fully(if wide { 8 } else { 4 })?;
        position += entry_size;
    }
    entries.sort_by_key(|&(offset, _)| offset);
    for (offset, size) in entries {
        stream.skip_fully(offset - position)?;
        let size = usize::try_from(size).expect("a size within MAX_ARCH_SIZE");
        found.push(stream.read_fully(size)?);
        position = offset + size as i64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    use extractous_core::large::LargeFile;

    use super::*;

    fn mach_o(cpu_type: i32, file_type: i32) -> Vec<u8> {
        let mut file = vec![0xCF, 0xFA, 0xED, 0xFE];
        for field in [cpu_type, 3, file_type, 0, 0, 0, 0] {
            file.extend_from_slice(&field.to_le_bytes());
        }
        file
    }

    fn pe(machine: u16, offset: u32) -> Vec<u8> {
        let mut file = vec![0u8; 0x200];
        file[..2].copy_from_slice(b"MZ");
        file[0x3c..0x40].copy_from_slice(&offset.to_le_bytes());
        let at = offset.max(0x40) as usize;
        file[at..at + 4].copy_from_slice(b"PE\0\0");
        file[at + 4..at + 6].copy_from_slice(&machine.to_le_bytes());
        file[at + 8..at + 12].copy_from_slice(&(-86_400i32).to_le_bytes());
        file
    }

    fn elf(class: u8, data: u8, os: u8, kind: u16, machine: u16) -> Vec<u8> {
        let mut file = vec![
            0x7F, b'E', b'L', b'F', class, data, 1, os, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        let field = |value: u16| {
            if data == 1 {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            }
        };
        file.extend_from_slice(&field(kind));
        file.extend_from_slice(&field(machine));
        file.extend_from_slice(&[0; 40]);
        file
    }

    fn universal(architectures: &[Vec<u8>], reversed: bool) -> Vec<u8> {
        let mut file = vec![0xCA, 0xFE, 0xBA, 0xBE];
        file.extend_from_slice(&u32::try_from(architectures.len()).unwrap().to_be_bytes());
        let mut offset = 8 + 20 * architectures.len();
        let mut entries = Vec::new();
        for architecture in architectures {
            let mut entry = vec![0u8; 8];
            entry.extend_from_slice(&u32::try_from(offset).unwrap().to_be_bytes());
            entry.extend_from_slice(&u32::try_from(architecture.len()).unwrap().to_be_bytes());
            entry.extend_from_slice(&[0, 0, 0, 12]);
            entries.push(entry);
            offset += architecture.len();
        }
        if reversed {
            entries.reverse();
        }
        file.extend(entries.concat());
        file.extend(architectures.concat());
        file
    }

    /// An executable too large to read whole is read as far as its header.
    #[test]
    fn reads_executables_too_large_to_read_whole() {
        let file = LargeFile::padded("long.so", &elf(2, 1, 0, 3, 0x3E));
        let metadata = file.extract_each(ExecutableParser.name(), |_| {});
        assert_eq!(metadata[ARCHITECTURE_BITS], ["64"]);
        assert_eq!(metadata[ENDIAN], [LITTLE]);
    }

    /// A universal binary too large to read whole hands each architecture on in place.
    #[test]
    fn reads_universal_binaries_too_large_to_read_whole() {
        let head = universal(&[mach_o(0x0100_0007, 2)], false);
        let file = LargeFile::padded("long.bundle", &head);
        file.extract_each(UniversalExecutableParser.name(), |_| {});
    }

    #[test]
    fn reads_no_coff_header_inside_the_ms_dos_header() {
        let (_, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(&pe(0x14c, 0x3f))
            .unwrap();
        assert_eq!(metadata[PLATFORM], ["Windows"]);
        assert!(!metadata.contains_key(MACHINE_TYPE), "{metadata:?}");
        let (_, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(&pe(0x14c, 0x40))
            .unwrap();
        assert_eq!(metadata[MACHINE_TYPE], ["x86-32"]);
    }
}
