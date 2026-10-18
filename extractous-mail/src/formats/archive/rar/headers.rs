//! The headers of a RAR archive as junrar 8.1.1's `Archive` reads them when Tika's
//! `RarParser` opens it: the marker found, then `readHeaders3`'s walk over the blocks of a RAR
//! 1.5 to 4 archive, which skips a block it cannot parse and keeps the headers it has read
//! where it stops, and the RAR 3 `FileHeader` of each file.

use std::collections::HashSet;

/// `MAX_HEADER_SIZE`: the most bytes a header body may declare.
const MAX_HEADER_SIZE: i64 = 20 * 1024 * 1024;
/// `MAXSFXSIZE`: how far past its start a self-extracting archive's marker may be.
const MAX_SFX_SIZE: usize = 0x40_0000;
const BASE_BLOCK_SIZE: i64 = 7;
const BLOCK_HEADER_SIZE: i64 = 4;
/// `FileHeader.NEWLHD_SIZE`.
const NEWLHD_SIZE: i64 = 32;
/// The longest name, and name component, `new File(name).getCanonicalPath()` is sure to
/// accept: junrar skips the header of a name it rejects, and how long a path it accepts
/// depends on the length of the JVM's working directory.
const MAX_SURE_NAME: usize = 1024;
const MAX_SURE_COMPONENT: usize = 255;

const MHD_PASSWORD: u16 = 0x0080;
const MHD_ENCRYPTVER: u16 = 0x0200;
const LHD_SPLIT_BEFORE: u16 = 0x0001;
const LHD_SPLIT_AFTER: u16 = 0x0002;
const LHD_PASSWORD: u16 = 0x0004;
const LHD_COMMENT: u16 = 0x0008;
const LHD_SOLID: u16 = 0x0010;
const LHD_WINDOWMASK: u16 = 0x00e0;
const LHD_DIRECTORY: u16 = 0x00e0;
const LHD_LARGE: u16 = 0x0100;
const LHD_UNICODE: u16 = 0x0200;
const LHD_SALT: u16 = 0x0400;
const LHD_EXTTIME: u16 = 0x1000;

/// Why junrar or Tika's parser does not open an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Unopened {
    /// Opening it throws, failing the document, as an encrypted archive's
    /// `EncryptedDocumentException` does when `encrypted`.
    Fails { encrypted: bool, reason: String },
    /// junrar reads it in a way the port does not.
    Unported(String),
}

/// A file entry's name as junrar decodes it, its separators, which the format writes as
/// backslashes whatever the host, turned into slashes, as Tika's `handleEntryMetadata` and
/// unrar turn them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Name {
    Decoded(String),
    /// A name the port may not take as junrar does, for this reason, as the port takes it.
    Differs(String, &'static str),
}

/// The RAR 3 `FileHeader` of a file entry, as far as Tika's parser and the extraction read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileHeader {
    position: i64,
    header_size: i16,
    flags: u16,
    pub(super) full_pack_size: i64,
    pub(super) full_unpack_size: i64,
    pub(super) file_crc: u32,
    /// `getUnpVersion()`, a signed `byte`.
    pub(super) unp_version: i32,
    pub(super) unp_method: u8,
    pub(super) name: Name,
    /// `getMTime()` and `getCTime()` in milliseconds since the epoch, their DOS times read as
    /// wall-clock times in UTC, none where a DOS time is out of range.
    pub(super) modified_millis: Option<i64>,
    pub(super) created_millis: Option<i64>,
    /// Whether the header announced fields it does not hold, or its CRC does not match,
    /// which junrar refuses to extract.
    pub(super) broken: bool,
}

impl FileHeader {
    pub(super) fn is_directory(&self) -> bool {
        self.flags & LHD_WINDOWMASK == LHD_DIRECTORY
    }

    pub(super) fn is_solid(&self) -> bool {
        self.flags & LHD_SOLID != 0
    }

    pub(super) fn is_encrypted(&self) -> bool {
        self.flags & LHD_PASSWORD != 0
    }

    pub(super) fn is_split(&self) -> bool {
        self.flags & (LHD_SPLIT_BEFORE | LHD_SPLIT_AFTER) != 0
    }

    /// `getDataStartOffset(false)`: where the packed data starts, which a header size past
    /// 32767 makes negative.
    pub(super) fn data_start(&self) -> i64 {
        self.position + i64::from(self.header_size)
    }
}

/// The headers of an archive Tika's parser goes on with: not encrypted, with a main header.
#[derive(Debug)]
pub(super) struct Headers {
    /// `getFileHeaders()`: the file entries, in archive order.
    pub(super) files: Vec<FileHeader>,
}

/// `Archive`'s constructor and `isEncrypted`, as `RarParser` calls them.
pub(super) fn read(data: &[u8]) -> Result<Headers, Unopened> {
    match signature_type(data) {
        Signature::Rar15 => {}
        Signature::Rar50 => {
            return Err(fails(
                "UnsupportedRarV5Exception: a RAR 5 archive, which Tika detects as such only \
                 with its full marker",
            ))
        }
        Signature::Rar14 => return Err(unported("a RAR 1.4 archive")),
        Signature::Future => {
            return Err(fails(
                "UnsupportedRarVersionException: a RAR format junrar predates",
            ))
        }
        Signature::None => {
            let scanned = &data[1.min(data.len())..data.len().min(MAX_SFX_SIZE + 1)];
            if (0..scanned.len()).any(|i| sfx_signature(scanned, i)) {
                return Err(unported(
                    "a RAR marker past the start, after a self-extractor's code",
                ));
            }
            return Err(fails("BadRarArchiveException: no RAR marker"));
        }
    }
    let walk = Walk::run(data);
    let main = walk
        .main
        .ok_or_else(|| fails("MainHeaderNullException: no main header"))?;
    if main & MHD_PASSWORD != 0 {
        return Err(Unopened::Fails {
            encrypted: true,
            reason: "EncryptedDocumentException: the archive's headers are encrypted".to_owned(),
        });
    }
    Ok(Headers { files: walk.files })
}

fn fails(reason: &str) -> Unopened {
    Unopened::Fails {
        encrypted: false,
        reason: reason.to_owned(),
    }
}

fn unported(what: &str) -> Unopened {
    Unopened::Unported(what.to_owned())
}

/// `signatureType`'s classes of a marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signature {
    None,
    Rar14,
    Rar15,
    Rar50,
    Future,
}

/// `signatureType` of the bytes `d`.
fn signature_type(d: &[u8]) -> Signature {
    if d.first() != Some(&0x52) {
        return Signature::None;
    }
    if d.len() >= 4 && d[1..4] == [0x45, 0x7e, 0x5e] {
        return Signature::Rar14;
    }
    if d.len() >= 7 && d[1..6] == [0x61, 0x72, 0x21, 0x1a, 0x07] {
        return match d[6] {
            0 => Signature::Rar15,
            1 => Signature::Rar50,
            2..=4 => Signature::Future,
            _ => Signature::None,
        };
    }
    Signature::None
}

/// Whether `detectFormatAndSeek`'s scan takes a marker at `i` of the bytes after the first:
/// any but a RAR 1.4 one past the first byte that the `RSFX` mark at offset 28 does not
/// corroborate.
fn sfx_signature(buffer: &[u8], i: usize) -> bool {
    match signature_type(&buffer[i..]) {
        Signature::None => false,
        Signature::Rar14 => i == 0 || buffer.len() <= 31 || buffer[27..31] == *b"RSFX",
        _ => true,
    }
}

/// `BaseBlock`: the seven bytes every RAR 3 block starts with.
#[derive(Debug, Clone, Copy)]
struct BaseBlock {
    head_crc: u16,
    header_type: u8,
    flags: u16,
    header_size: i16,
}

/// `UnrarHeadertype`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeaderType {
    Main,
    Mark,
    File,
    Comment,
    Av,
    Sub,
    Protect,
    Sign,
    NewSub,
    EndArc,
}

impl HeaderType {
    fn find(byte: u8) -> Option<Self> {
        Some(match byte {
            0x73 => HeaderType::Main,
            0x72 => HeaderType::Mark,
            0x74 => HeaderType::File,
            0x75 => HeaderType::Comment,
            0x76 => HeaderType::Av,
            0x77 => HeaderType::Sub,
            0x78 => HeaderType::Protect,
            0x79 => HeaderType::Sign,
            0x7a => HeaderType::NewSub,
            0x7b => HeaderType::EndArc,
            _ => return None,
        })
    }
}

/// How a step of the walk ends: on to the next block, or the walk over, keeping what it has
/// read, as every exception `readHeaders3` throws on an archive whose headers are not
/// encrypted ends it, caught or swallowed.
enum Step {
    Next,
    Stop,
}

/// `readHeaders3`'s walk.
struct Walk<'a> {
    data: &'a [u8],
    position: i64,
    processed: HashSet<i64>,
    /// The flags of the last main header read.
    main: Option<u16>,
    files: Vec<FileHeader>,
}

impl<'a> Walk<'a> {
    fn run(data: &'a [u8]) -> Self {
        let mut walk = Walk {
            data,
            position: 0,
            processed: HashSet::new(),
            main: None,
            files: Vec::new(),
        };
        while let Step::Next = walk.step() {}
        walk
    }

    fn length(&self) -> i64 {
        self.data.len() as i64
    }

    /// `readFully` of `count` bytes, or `None` where `RandomAccessFile` throws at the end.
    fn read(&mut self, count: i64) -> Option<&'a [u8]> {
        let start = usize::try_from(self.position).ok()?;
        let end = start.checked_add(usize::try_from(count).ok()?)?;
        let bytes = self.data.get(start..end)?;
        self.position += count;
        Some(bytes)
    }

    /// `advanceTo`: on to `next`, unless it does not move past the block at `from`.
    fn advance_to(&mut self, next: i64, from: i64) -> Step {
        if next <= from || !self.processed.insert(next) {
            return Step::Stop;
        }
        self.position = next;
        Step::Next
    }

    fn step(&mut self) -> Step {
        if self.main.is_some_and(|flags| flags & MHD_PASSWORD != 0) {
            return Step::Stop;
        }
        let position = self.position;
        if position >= self.length() {
            return Step::Stop;
        }
        let Some(bytes) = self.read(BASE_BLOCK_SIZE) else {
            return Step::Stop;
        };
        let block = BaseBlock {
            head_crc: u16::from_le_bytes([bytes[0], bytes[1]]),
            header_type: bytes[2],
            flags: u16::from_le_bytes([bytes[3], bytes[4]]),
            header_size: i16::from_le_bytes([bytes[5], bytes[6]]),
        };
        let Some(header_type) = HeaderType::find(block.header_type) else {
            return self.advance_to(position + i64::from(block.header_size), position);
        };
        match header_type {
            HeaderType::Mark => {
                let signature = [
                    bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                ];
                let valid =
                    block.head_crc == 0x6152 && block.flags == 0x1a21 && block.header_size == 7;
                if matches!(
                    signature_type(&signature),
                    Signature::Rar14 | Signature::Rar15
                ) && valid
                {
                    Step::Next
                } else {
                    Step::Stop
                }
            }
            HeaderType::Main => {
                let size = if block.flags & MHD_ENCRYPTVER != 0 {
                    7
                } else {
                    6
                };
                if self.read(size).is_none() {
                    return Step::Stop;
                }
                self.main = Some(block.flags);
                Step::Next
            }
            HeaderType::Sign => self.skip_body(8),
            HeaderType::Av => self.skip_body(7),
            HeaderType::Comment => match self.read(6) {
                Some(_) => self.advance_to(position + i64::from(block.header_size), position),
                None => Step::Stop,
            },
            HeaderType::EndArc => Step::Stop,
            HeaderType::File | HeaderType::NewSub | HeaderType::Protect | HeaderType::Sub => {
                let Some(pack) = self.read(BLOCK_HEADER_SIZE) else {
                    return Step::Stop;
                };
                let pack_size = i64::from(u32::from_le_bytes([pack[0], pack[1], pack[2], pack[3]]));
                match header_type {
                    HeaderType::Sub => self.sub_block(position, block, pack_size),
                    HeaderType::Protect => match self.clamped_body(block) {
                        Some(_) => self.advance_to(
                            position + i64::from(block.header_size) + pack_size,
                            position,
                        ),
                        None => Step::Stop,
                    },
                    _ => self.file_block(position, block, header_type, pack, pack_size),
                }
            }
        }
    }

    /// A block whose fixed body of `size` bytes junrar reads without moving past the rest.
    fn skip_body(&mut self, size: i64) -> Step {
        match self.read(size) {
            Some(_) => Step::Next,
            None => Step::Stop,
        }
    }

    /// The body after the block header, as much of what the header declares as the archive
    /// holds; `None` past `MAX_HEADER_SIZE`, where `safelyAllocate` throws.
    fn clamped_body(&mut self, block: BaseBlock) -> Option<&'a [u8]> {
        let declared = i64::from(block.header_size) - BASE_BLOCK_SIZE - BLOCK_HEADER_SIZE;
        let size = declared.min(self.length() - self.position).max(0);
        if size > MAX_HEADER_SIZE {
            return None;
        }
        self.read(size)
    }

    fn sub_block(&mut self, position: i64, block: BaseBlock, pack_size: i64) -> Step {
        let Some(sub) = self.read(3) else {
            return Step::Stop;
        };
        let read = match u16::from_le_bytes([sub[0], sub[1]]) {
            0x102 => 8,
            0x100 => 10,
            0x101 => {
                let size = i64::from(block.header_size) - BASE_BLOCK_SIZE - BLOCK_HEADER_SIZE - 3;
                if !(0..=MAX_HEADER_SIZE).contains(&size) {
                    return Step::Stop;
                }
                size
            }
            0x103..=0x105 => 0,
            _ => return Step::Next,
        };
        if self.read(read).is_none() {
            return Step::Stop;
        }
        self.advance_to(
            position + i64::from(block.header_size) + pack_size,
            position,
        )
    }

    fn file_block(
        &mut self,
        position: i64,
        block: BaseBlock,
        header_type: HeaderType,
        pack: &[u8],
        pack_size: i64,
    ) -> Step {
        let Some(body) = self.clamped_body(block) else {
            return Step::Stop;
        };
        let file = header_type == HeaderType::File;
        let Ok((mut header, parsed_length)) = parse_file_header(block, file, pack_size, body)
        else {
            return self.advance_to(
                position + i64::from(block.header_size) + pack_size,
                position,
            );
        };
        header.position = position;
        let start = position as usize;
        let mut covered = Vec::with_capacity(11 + body.len());
        covered.extend_from_slice(&self.data[start..start + 7]);
        covered.extend_from_slice(pack);
        covered.extend_from_slice(body);
        if file && block.flags & LHD_COMMENT != 0 {
            let narrow = 11 + parsed_length;
            header.broken |= header_crc(&covered[..narrow]) != block.head_crc;
        }
        header.broken |= header_crc(&covered) != block.head_crc;
        let next = position + i64::from(block.header_size) + header.full_pack_size;
        if header_type == HeaderType::File {
            self.files.push(header);
        }
        self.advance_to(next, position)
    }
}

/// `RarCRC.computeHeaderCrc16` over a header's bytes after its CRC, which junrar checks
/// against the whole header it read.
fn header_crc(header: &[u8]) -> u16 {
    if header.len() <= 2 {
        return 0;
    }
    crate::compress::crc32(&header[2..]) as u16
}

/// The RAR 3 `FileHeader` constructor over the header body after the block header, and how
/// much of the body it parsed; `Err` where it throws `CorruptHeaderException`.
fn parse_file_header(
    block: BaseBlock,
    file: bool,
    pack_size: i64,
    body: &[u8],
) -> Result<(FileHeader, usize), ()> {
    let flags = block.flags;
    if (body.len() as i64) < NEWLHD_SIZE - BASE_BLOCK_SIZE - BLOCK_HEADER_SIZE {
        return Err(());
    }
    let int_at =
        |at: usize| i32::from_le_bytes([body[at], body[at + 1], body[at + 2], body[at + 3]]);
    let unp_size = i64::from(int_at(0) as u32);
    let file_crc = int_at(5) as u32;
    let file_time = int_at(9);
    let unp_version = i32::from(body[13] as i8);
    let unp_method = body[14];
    let mut name_size = i16::from_le_bytes([body[15], body[16]]);
    let mut position = 21usize;
    let (high_pack, high_unpack) = if flags & LHD_LARGE != 0 {
        if body.len() - position < 8 {
            return Err(());
        }
        position += 8;
        (int_at(21), int_at(25))
    } else if unp_size == 0xffff_ffff {
        (0, i32::MAX)
    } else {
        (0, 0)
    };
    let full_pack_size = (i64::from(high_pack) << 32) | pack_size;
    let full_unpack_size = (i64::from(high_unpack) << 32).wrapping_add(unp_size);
    if full_pack_size < 0 || full_unpack_size < 0 {
        return Err(());
    }
    let mut broken = false;
    name_size = name_size.min(4 * 1024);
    if name_size <= 0 {
        return Err(());
    }
    let remaining = body.len() - position;
    let mut name_size = name_size as usize;
    if name_size > remaining {
        name_size = remaining;
        broken = true;
    }
    if name_size == 0 {
        return Err(());
    }
    let name_bytes = &body[position..position + name_size];
    position += name_size;
    let name = if file {
        match checked_name(&file_name(flags, name_bytes)?)? {
            Name::Decoded(name) => Name::Decoded(name.replace('\\', "/")),
            unported => unported,
        }
    } else {
        Name::Decoded(String::new())
    };
    if block.header_type == 0x7a {
        let mut data_size = i64::from(block.header_size) - NEWLHD_SIZE - name_size as i64;
        if flags & LHD_SALT != 0 {
            data_size -= 8;
        }
        if flags & LHD_LARGE != 0 {
            data_size -= 8;
        }
        let remaining = (body.len() - position) as i64;
        if data_size > remaining {
            data_size = remaining;
            broken = true;
        }
        if data_size > 0 {
            position += data_size as usize;
        }
    }
    if flags & LHD_SALT != 0 {
        if body.len() - position < 8 {
            return Err(());
        }
        position += 8;
    }
    let mut modified_millis = dos_millis(file_time);
    let mut created_millis = None;
    if flags & LHD_EXTTIME != 0 {
        let ext_flags = if position + 1 < body.len() {
            let ext_flags = u16::from_le_bytes([body[position], body[position + 1]]);
            position += 2;
            ext_flags
        } else {
            0
        };
        let mut times = ExtTimes {
            body,
            position,
            flags: ext_flags,
            broken: false,
        };
        modified_millis = times.next(12, Base::Header(modified_millis));
        created_millis = times.next(8, Base::Own);
        times.next(4, Base::Own);
        times.next(0, Base::Own);
        broken |= times.broken;
    }
    let header = FileHeader {
        position: 0,
        header_size: block.header_size,
        flags,
        full_pack_size,
        full_unpack_size,
        file_crc,
        unp_version,
        unp_method,
        name,
        modified_millis,
        created_millis,
        broken,
    };
    Ok((header, position.min(body.len())))
}

/// The name `getFileName()` gives, which `isFilenameValid` rejects with a NUL in it, as
/// `File.getCanonicalPath` does.
fn checked_name(name: &[u16]) -> Result<Name, ()> {
    if name.contains(&0) {
        return Err(());
    }
    let Ok(name) = String::from_utf16(name) else {
        return Ok(Name::Differs(
            String::from_utf16_lossy(name),
            "a name with an unpaired surrogate, which the port replaces as Java's encoder does \
             not",
        ));
    };
    if name.len() > MAX_SURE_NAME || name.split('/').any(|part| part.len() > MAX_SURE_COMPONENT) {
        return Ok(Name::Differs(
            name,
            "a name so long that whether junrar takes it for a path depends on the JVM's \
             working directory, which the port takes it for",
        ));
    }
    Ok(Name::Decoded(name))
}

/// `parseExtTime` of the extended times after a file header's name, in order.
struct ExtTimes<'a> {
    body: &'a [u8],
    position: usize,
    flags: u16,
    broken: bool,
}

/// The whole seconds an extended time refines: the header's own DOS time, or a DOS time of its
/// own that precedes it.
#[derive(Debug, Clone, Copy)]
enum Base {
    Header(Option<i64>),
    Own,
}

impl ExtTimes<'_> {
    /// The time of the four bits at `shift` in milliseconds since the epoch: the header's time
    /// where an extended modification time is not there, none where another is not there or
    /// its DOS time is out of range.
    fn next(&mut self, shift: u32, base: Base) -> Option<i64> {
        let flag = self.flags >> shift;
        if flag & 0x8 == 0 {
            return match base {
                Base::Header(millis) => millis,
                Base::Own => None,
            };
        }
        let base_millis = match base {
            Base::Header(millis) => millis,
            Base::Own => {
                if self.body.len() - self.position < 4 {
                    self.broken = true;
                    return None;
                }
                let at = self.position;
                let time = i32::from_le_bytes([
                    self.body[at],
                    self.body[at + 1],
                    self.body[at + 2],
                    self.body[at + 3],
                ]);
                self.position += 4;
                dos_millis(time)
            }
        };
        let count = usize::from(flag & 0x3);
        let mut remainder = 0i64;
        if count > self.body.len() - self.position {
            self.broken = true;
            self.position = self.body.len();
        } else {
            for _ in 0..count {
                let byte = i64::from(self.body[self.position]);
                remainder = (byte << 16) | (remainder >> 8);
                self.position += 1;
            }
        }
        let seconds = base_millis? / 1000;
        let mut nanos = remainder * 100;
        if flag & 0x4 != 0 {
            nanos += 1_000_000_000;
        }
        let seconds = seconds + nanos.div_euclid(1_000_000_000);
        let nanos = nanos.rem_euclid(1_000_000_000);
        Some(seconds * 1000 + nanos / 1_000_000)
    }
}

/// `getDateDos`: a DOS date and time, read as a wall-clock time in UTC rather than in the JVM's
/// default zone by a lenient `Calendar`, as junrar reads it (#122).
fn dos_millis(time: i32) -> Option<i64> {
    crate::compress::dos_millis(i64::from(time as u32))
}

/// `getFileName()` of a file header, as UTF-16: the Unicode name after the first NUL of a
/// header that has one, decoded by `FileNameDecoder`, else its narrow name; `Err` where the
/// encoded name is cut short.
fn file_name(flags: u16, bytes: &[u8]) -> Result<Vec<u16>, ()> {
    if flags & LHD_UNICODE == 0 {
        return Ok(utf16(&narrow_name(bytes)));
    }
    let length = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let wide = if length != bytes.len() {
        decode_unicode_name(bytes, length, length + 1)?
    } else {
        utf16(&narrow_name(bytes))
    };
    if wide.is_empty() {
        return Ok(utf16(&narrow_name(&bytes[..length])));
    }
    Ok(wide)
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// `decodeNarrowName`: ASCII, else UTF-8 where it decodes, else ISO-8859-1.
fn narrow_name(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => crate::java::latin1(bytes),
    }
}

/// `FileNameDecoder.decode`: the UTF-16 name encoded after the narrow name of
/// `narrow_size` bytes, from `enc_pos` on.
fn decode_unicode_name(
    name: &[u8],
    narrow_size: usize,
    mut enc_pos: usize,
) -> Result<Vec<u16>, ()> {
    let byte = |at: usize| name.get(at).copied().map(u16::from).ok_or(());
    let mut dec_pos = 0usize;
    let mut flags = 0u16;
    let mut flag_bits = 0;
    let high_byte = if enc_pos < name.len() {
        enc_pos += 1;
        byte(enc_pos - 1)?
    } else {
        0
    };
    let mut decoded = Vec::new();
    while enc_pos < name.len() {
        if flag_bits == 0 {
            flags = byte(enc_pos)?;
            enc_pos += 1;
            flag_bits = 8;
        }
        match flags >> 6 {
            0 => {
                decoded.push(byte(enc_pos)?);
                enc_pos += 1;
                dec_pos += 1;
            }
            1 => {
                decoded.push(byte(enc_pos)?.wrapping_add(high_byte << 8));
                enc_pos += 1;
                dec_pos += 1;
            }
            2 => {
                if enc_pos + 1 >= name.len() {
                    return Err(());
                }
                decoded.push((byte(enc_pos + 1)? << 8).wrapping_add(byte(enc_pos)?));
                dec_pos += 1;
                enc_pos += 2;
            }
            _ => {
                let mut length = byte(enc_pos)?;
                enc_pos += 1;
                if length & 0x80 != 0 {
                    let correction = byte(enc_pos)?;
                    enc_pos += 1;
                    length = (length & 0x7f) + 2;
                    while length > 0 && dec_pos < narrow_size {
                        let low = (byte(dec_pos)? + correction) & 0xff;
                        decoded.push((high_byte << 8).wrapping_add(low));
                        length -= 1;
                        dec_pos += 1;
                    }
                } else {
                    length += 2;
                    while length > 0 && dec_pos < narrow_size {
                        decoded.push(byte(dec_pos)?);
                        length -= 1;
                        dec_pos += 1;
                    }
                }
            }
        }
        flags = (flags << 2) & 0xff;
        flag_bits -= 2;
    }
    Ok(decoded)
}
