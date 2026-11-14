//! Flash movies (`.swf`), which Tika does not read, read for their text: uncompressed (`FWS`),
//! zlib-compressed (`CWS`) or LZMA-compressed (`ZWS`).
//!
//! The text is written in the order its characters are defined in the movie's tags, each text
//! character once, whichever frames place it and whether any does: the depths of the display
//! list stack characters for drawing rather than order them for reading, and change frame by
//! frame. A sprite's tags are read where the sprite is defined.
//!
//! - A static text (`DefineText`, `DefineText2`) is a paragraph, its glyphs mapped to characters
//!   by the code table of their font ([`text`]): that of `DefineFont2` or `DefineFont3`, or the
//!   `DefineFontInfo` or `DefineFontInfo2` of a `DefineFont`. A text record that moves to
//!   another Y offset starts a new line, and one that moves along the line by more than a fifth
//!   of its text height past where the glyphs before it end is a space apart from them. The text
//!   set in a font the movie does not define, or that has no code table, is not written, and the
//!   font is named in a warning.
//! - A dynamic or input text field (`DefineEditText`) is written with its initial text: plain
//!   text a paragraph, its lines broken where it breaks them, HTML text ([`html`]) a paragraph
//!   for each of its paragraphs and a list for its list items, its links written as their text.
//!   The text of a password field, which Flash shows masked, is not written.
//!
//! Strings and the codes of fonts are Unicode from SWF 6 on; before, a string is windows-1252
//! and a code windows-1252, or Shift_JIS where its font says so.
//!
//! The header's version, frame rate and frame count are `swf:version`, `swf:frameRate` and
//! `swf:frameCount`, the `FileAttributes`' ActionScript 3 flag `swf:actionScript3`, and the
//! XMP of the `Metadata` tag is read as XMP is, into Dublin Core and the rest; XMP that does not
//! read is named in a warning.
//!
//! Images and sounds are handed to the embedded document extractor ([`media`]), each named by
//! its character id: the JPEG of `DefineBits` (with the movie's `JPEGTables`), of
//! `DefineBitsJPEG2`, `DefineBitsJPEG3` and `DefineBitsJPEG4` without its alpha channel, or the
//! PNG or GIF these hold instead, and the MP3 of `DefineSound`. Lossless bitmaps, which hold
//! bare pixels, sounds in other formats and streamed sound are not handed on, and a warning
//! says so.
//!
//! The header's file length bounds the movie: a compressed body is decoded up to the length it
//! declares, and never past [`decode_limit`](crate::compress::decode_limit), and an
//! uncompressed one read up to it. A body that ends before its length is read as far as its
//! tags go, and a tag cut short fails the movie.

mod html;
mod media;
mod reader;
mod text;

use std::collections::HashSet;
use std::io::Read;

use flate2::bufread::ZlibDecoder;

use crate::compress::{xz, Decoding, Limited, Undecoded};
use crate::config::ParserConfigs;
use crate::errors::Error;
use crate::formats::embedded::EXCEPTION_WARNING;
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::{Stop, Xhtml};
use crate::formats::xmp;
use crate::formats::{Decline, Failure, Parser, Prepared};
use crate::input::Input;
use crate::text::charset::Charset;

use reader::{Bytes, Short};
use text::Fonts;

const NAME: &str = "dijo::SwfParser";
const WHAT: &str = "The SWF movie";

/// The bytes of the header every movie starts with: its signature, version and file length.
const HEADER_BYTES: usize = 8;
/// Where the LZMA data of a `ZWS` movie starts, after its header, the length of its compressed
/// data and its LZMA properties.
const LZMA_DATA: usize = 17;

const END: u16 = 0;
const DEFINE_BITS: u16 = 6;
const JPEG_TABLES: u16 = 8;
const DEFINE_FONT: u16 = 10;
const DEFINE_TEXT: u16 = 11;
const DEFINE_FONT_INFO: u16 = 13;
const DEFINE_SOUND: u16 = 14;
const SOUND_STREAM_BLOCK: u16 = 19;
const DEFINE_BITS_LOSSLESS: u16 = 20;
const DEFINE_BITS_JPEG_2: u16 = 21;
const DEFINE_TEXT_2: u16 = 33;
const DEFINE_BITS_JPEG_3: u16 = 35;
const DEFINE_BITS_LOSSLESS_2: u16 = 36;
const DEFINE_EDIT_TEXT: u16 = 37;
const DEFINE_SPRITE: u16 = 39;
const DEFINE_FONT_2: u16 = 48;
const DEFINE_FONT_INFO_2: u16 = 62;
const FILE_ATTRIBUTES: u16 = 69;
const DEFINE_FONT_3: u16 = 75;
const METADATA: u16 = 77;
const DEFINE_BITS_JPEG_4: u16 = 90;

/// The `FileAttributes` flag of a movie whose scripts are ActionScript 3.
const ACTION_SCRIPT_3: u32 = 0x08;
/// The first version of SWF whose strings and font codes are Unicode.
const UNICODE_VERSION: u8 = 6;

/// The text of Flash movies.
pub struct SwfParser;

impl Parser for SwfParser {
    fn name(&self) -> &'static str {
        NAME
    }
    fn tika_name(&self) -> &'static str {
        NAME
    }

    fn media_types(&self) -> &'static [&'static str] {
        &["application/x-shockwave-flash"]
    }

    /// Fails a file whose header is not that of a movie before anything is written.
    fn prepare<'a>(
        &self,
        input: &'a [u8],
        _metadata: &TikaMetadata,
        _configs: &'a ParserConfigs,
    ) -> Result<Prepared<'a>, Decline<'a>> {
        let header =
            Header::read(input).map_err(|reason| Decline::Fails(Error::ParseError(reason)))?;
        Ok(Box::new(move |xhtml| {
            let body = header.body(input)?;
            read_movie(&body, header.version, xhtml)
        }))
    }
}

/// How a movie's body is compressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compression {
    /// `FWS`.
    None,
    /// `CWS`.
    Zlib,
    /// `ZWS`, with the LZMA properties byte and dictionary size.
    Lzma { properties: u8, dictionary: u32 },
}

/// A movie's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Header {
    compression: Compression,
    version: u8,
    /// The length of the movie uncompressed, the header's eight bytes included.
    length: u32,
}

impl Header {
    fn read(input: &[u8]) -> Result<Header, String> {
        let header = input.get(..HEADER_BYTES).ok_or_else(|| {
            format!(
                "{WHAT} ends within its {HEADER_BYTES}-byte header, after {} bytes",
                input.len()
            )
        })?;
        let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        if usize::try_from(length).is_ok_and(|length| length < HEADER_BYTES) {
            return Err(format!(
                "{WHAT} declares a length of {length} bytes, shorter than its header"
            ));
        }
        let compression = match &header[..3] {
            b"FWS" => Compression::None,
            b"CWS" => Compression::Zlib,
            b"ZWS" => {
                let lzma = input.get(HEADER_BYTES..LZMA_DATA).ok_or_else(|| {
                    format!("{WHAT} ends within the LZMA properties of its header")
                })?;
                Compression::Lzma {
                    properties: lzma[4],
                    dictionary: u32::from_le_bytes([lzma[5], lzma[6], lzma[7], lzma[8]]),
                }
            }
            signature => {
                return Err(format!(
                    "{WHAT} starts with {:?}, not FWS, CWS or ZWS",
                    String::from_utf8_lossy(signature)
                ))
            }
        };
        Ok(Header {
            compression,
            version: header[3],
            length,
        })
    }

    /// The movie's body after its header: `input`'s own bytes, or decoded, up to the length
    /// the header declares.
    fn body<'a>(&self, input: &'a [u8]) -> Result<Input<'a>, Failure> {
        let declared = u64::from(self.length) - HEADER_BYTES as u64;
        match self.compression {
            Compression::None => {
                let end =
                    usize::try_from(self.length).map_or(input.len(), |end| end.min(input.len()));
                Ok(Input::Borrowed(&input[HEADER_BYTES..end]))
            }
            Compression::Zlib => {
                let data = &input[HEADER_BYTES..];
                decoded(data.len(), declared, ZlibDecoder::new(data))
            }
            Compression::Lzma {
                properties,
                dictionary,
            } => {
                let data = &input[LZMA_DATA..];
                let reader = xz::raw_lzma_reader(data, properties, dictionary, declared)
                    .map_err(undecoded)?;
                decoded(data.len(), declared, reader)
            }
        }
    }
}

/// What `reader`, the decoder of a body of `compressed` bytes, decodes into, up to `declared`
/// bytes.
fn decoded(compressed: usize, declared: u64, reader: impl Read) -> Result<Input<'static>, Failure> {
    let mut decoding = Decoding::spooled(compressed, Some(declared));
    decoding
        .read_from(reader.take(declared))
        .map_err(undecoded)?;
    decoding
        .finish()
        .and_then(Limited::whole)
        .map_err(undecoded)
}

/// The failure of a movie whose compressed body is not decoded, as `undecoded` says.
fn undecoded(undecoded: Undecoded) -> Failure {
    let reason = format!("{WHAT}'s compressed body does not decode: {undecoded}");
    match undecoded {
        Undecoded::TooLarge | Undecoded::MemoryLimit(_) => {
            Failure::Extraction(Error::LimitExceeded(reason))
        }
        Undecoded::Unspooled(_) => Failure::Extraction(Error::IoError(reason)),
        Undecoded::Fails(_)
        | Undecoded::Unported(_)
        | Undecoded::Encrypted
        | Undecoded::BadPassword => Failure::Malformed(reason),
    }
}

/// The tags the parser reads and their names.
const NAMES: [(u16, &str); 20] = [
    (DEFINE_BITS, "DefineBits"),
    (JPEG_TABLES, "JPEGTables"),
    (DEFINE_FONT, "DefineFont"),
    (DEFINE_TEXT, "DefineText"),
    (DEFINE_FONT_INFO, "DefineFontInfo"),
    (DEFINE_SOUND, "DefineSound"),
    (SOUND_STREAM_BLOCK, "SoundStreamBlock"),
    (DEFINE_BITS_LOSSLESS, "DefineBitsLossless"),
    (DEFINE_BITS_JPEG_2, "DefineBitsJPEG2"),
    (DEFINE_TEXT_2, "DefineText2"),
    (DEFINE_BITS_JPEG_3, "DefineBitsJPEG3"),
    (DEFINE_BITS_LOSSLESS_2, "DefineBitsLossless2"),
    (DEFINE_EDIT_TEXT, "DefineEditText"),
    (DEFINE_SPRITE, "DefineSprite"),
    (DEFINE_FONT_2, "DefineFont2"),
    (DEFINE_FONT_INFO_2, "DefineFontInfo2"),
    (FILE_ATTRIBUTES, "FileAttributes"),
    (DEFINE_FONT_3, "DefineFont3"),
    (METADATA, "Metadata"),
    (DEFINE_BITS_JPEG_4, "DefineBitsJPEG4"),
];

/// A tag of the movie: its code, where it starts in the body, and its own bytes, which start
/// at `body_at`.
#[derive(Debug, Clone, Copy)]
struct Tag<'d> {
    code: u16,
    at: usize,
    body: &'d [u8],
    body_at: usize,
}

impl<'d> Tag<'d> {
    /// The failure of a movie whose tag `code` at byte `at` of the body is malformed, as
    /// `reason` says.
    fn failure(code: u16, at: usize, reason: impl std::fmt::Display) -> Failure {
        let tag = match NAMES.iter().find(|&&(known, _)| known == code) {
            Some((_, name)) => format!("{name} tag"),
            None => format!("tag of code {code}"),
        };
        Failure::Malformed(format!("{WHAT}'s {tag} at byte {at} of its body {reason}"))
    }

    /// The failure of a movie whose tag is malformed, as `reason` says.
    fn malformed(&self, reason: impl std::fmt::Display) -> Failure {
        Self::failure(self.code, self.at, reason)
    }

    /// The failure of a movie whose tag ends before its `what`.
    fn short(&self, Short(what): Short) -> Failure {
        self.malformed(format_args!("ends before its {what}"))
    }

    /// The tag's bytes, read in turn.
    fn bytes(&self) -> Bytes<'d> {
        Bytes::new(self.body)
    }
}

/// Writes the movie whose header has `version` from its `body`.
fn read_movie(body: &[u8], version: u8, xhtml: &mut Xhtml) -> Result<(), Failure> {
    let mut bytes = Bytes::new(body);
    let header = |Short(what): Short| {
        Failure::Malformed(format!(
            "{WHAT}'s body ends within its header, before its {what}"
        ))
    };
    bytes.skip_rect("frame size").map_err(header)?;
    let rate = bytes.u16("frame rate").map_err(header)?;
    let frames = bytes.u16("frame count").map_err(header)?;
    let metadata = xhtml.metadata_mut();
    metadata.set("swf:version", version.to_string());
    metadata.set("swf:frameRate", (f64::from(rate) / 256.0).to_string());
    metadata.set("swf:frameCount", frames.to_string());
    let at = bytes.at();
    let mut movie = Movie {
        xhtml,
        version,
        fonts: Fonts::default(),
        jpeg_tables: None,
        warned: HashSet::new(),
    };
    movie.tags(bytes.rest(), at, Nesting::Movie)
}

/// Whether tags are the movie's own or a sprite's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Nesting {
    Movie,
    Sprite,
}

/// What a warning is about, each given once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Warning {
    /// The text set in the font of this id.
    Font(u16),
    LosslessBitmaps,
    /// Sounds in the format of this number.
    SoundFormat(u8),
    StreamedSound,
}

/// A movie being written.
struct Movie<'d, 'x, 'h> {
    xhtml: &'x mut Xhtml<'h>,
    version: u8,
    fonts: Fonts<'d>,
    /// The `JPEGTables` the JPEG of each `DefineBits` is completed with.
    jpeg_tables: Option<&'d [u8]>,
    warned: HashSet<Warning>,
}

impl<'d> Movie<'d, '_, '_> {
    /// Reads the tags of `data`, which starts at byte `at` of the body, up to the `End` tag or
    /// the end of the data.
    fn tags(&mut self, data: &'d [u8], at: usize, nesting: Nesting) -> Result<(), Failure> {
        let mut bytes = Bytes::new(data);
        while !bytes.is_empty() {
            let start = at + bytes.at();
            let record = bytes.u16("record header").map_err(|_| {
                Failure::Malformed(format!(
                    "{WHAT} ends within the header of a tag at byte {start} of its body"
                ))
            })?;
            let code = record >> 6;
            let mut length = u32::from(record & 0x3F);
            if length == 0x3F {
                length = bytes.u32("long length").map_err(|Short(what)| {
                    Tag::failure(code, start, format_args!("ends before its {what}"))
                })?;
            }
            let length = usize::try_from(length).expect("a u32 fits a usize");
            let remaining = bytes.remaining();
            let body_at = at + bytes.at();
            let body = bytes.take(length, "body").map_err(|_| {
                Tag::failure(
                    code,
                    start,
                    format_args!("declares {length} bytes, more than the {remaining} left"),
                )
            })?;
            if code == END {
                return Ok(());
            }
            let tag = Tag {
                code,
                at: start,
                body,
                body_at,
            };
            self.tag(tag, nesting)?;
        }
        Ok(())
    }

    fn tag(&mut self, tag: Tag<'d>, nesting: Nesting) -> Result<(), Failure> {
        match tag.code {
            DEFINE_FONT => self.fonts.define_font(tag),
            DEFINE_FONT_INFO | DEFINE_FONT_INFO_2 => self.fonts.define_font_info(tag, self.version),
            DEFINE_FONT_2 | DEFINE_FONT_3 => self.fonts.define_font_2(tag, self.version),
            DEFINE_TEXT | DEFINE_TEXT_2 => self.define_text(tag),
            DEFINE_EDIT_TEXT => self.define_edit_text(tag),
            DEFINE_SPRITE => self.define_sprite(tag, nesting),
            JPEG_TABLES => {
                self.jpeg_tables = Some(tag.body);
                Ok(())
            }
            DEFINE_BITS | DEFINE_BITS_JPEG_2 | DEFINE_BITS_JPEG_3 | DEFINE_BITS_JPEG_4 => {
                self.define_bits(tag)
            }
            DEFINE_BITS_LOSSLESS | DEFINE_BITS_LOSSLESS_2 => {
                self.warn(Warning::LosslessBitmaps, || {
                    format!("{NAME} does not hand on the movie's lossless bitmaps")
                });
                Ok(())
            }
            DEFINE_SOUND => self.define_sound(tag),
            SOUND_STREAM_BLOCK => {
                self.warn(Warning::StreamedSound, || {
                    format!("{NAME} does not hand on the movie's streamed sound")
                });
                Ok(())
            }
            METADATA => {
                self.metadata(tag.body);
                Ok(())
            }
            FILE_ATTRIBUTES => {
                let flags = tag.bytes().u32("flags").map_err(|short| tag.short(short))?;
                let script_3 = flags & ACTION_SCRIPT_3 != 0;
                self.xhtml
                    .metadata_mut()
                    .set("swf:actionScript3", script_3.to_string());
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Reads the tags of a sprite where it is defined.
    fn define_sprite(&mut self, tag: Tag<'d>, nesting: Nesting) -> Result<(), Failure> {
        if nesting == Nesting::Sprite {
            return Err(tag.malformed("is nested in another sprite"));
        }
        let mut bytes = Bytes::new(tag.body);
        bytes
            .skip(4, "sprite id and frame count")
            .map_err(|short| tag.short(short))?;
        let start = tag.body_at + bytes.at();
        self.tags(bytes.rest(), start, Nesting::Sprite)
    }

    /// Reads the XMP of the `Metadata` tag `body` into the metadata.
    fn metadata(&mut self, body: &[u8]) {
        let packet = body.strip_suffix(&[0]).unwrap_or(body);
        if let Err(unported) = xmp::extract(packet, self.xhtml.metadata_mut(), xmp::as_is) {
            let warning = format!(
                "{NAME} does not read the movie's XMP metadata: {}",
                unported.0
            );
            self.xhtml.metadata_mut().add(EXCEPTION_WARNING, warning);
        }
    }

    /// Adds the warning `message` makes about `about` to the metadata, unless it was given.
    fn warn(&mut self, about: Warning, message: impl FnOnce() -> String) {
        if self.warned.insert(about) {
            self.xhtml.metadata_mut().add(EXCEPTION_WARNING, message());
        }
    }

    /// The text of the string `bytes` in the movie's version.
    fn string(&self, bytes: &[u8]) -> String {
        match self.version >= UNICODE_VERSION {
            true => String::from_utf8_lossy(bytes).into_owned(),
            false => windows_1252(bytes),
        }
    }
}

/// `bytes` decoded as windows-1252, the ANSI code page of strings before SWF 6.
fn windows_1252(bytes: &[u8]) -> String {
    Charset::named("windows-1252")
        .decode(bytes)
        .expect("the JDK decoders decode windows-1252")
}

/// Writes `lines` as a paragraph, a line break between each, leaving out empty lines; nothing
/// where all are empty.
fn write_lines<'l>(
    xhtml: &mut Xhtml,
    lines: impl IntoIterator<Item = &'l str>,
) -> Result<(), Stop> {
    let mut open = false;
    for line in lines.into_iter().filter(|line| !line.is_empty()) {
        match open {
            true => {
                xhtml.start_element("br")?;
                xhtml.end_element("br")?;
            }
            false => {
                xhtml.start_element("p")?;
                open = true;
            }
        }
        xhtml.characters(line)?;
    }
    if open {
        xhtml.end_element("p")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
