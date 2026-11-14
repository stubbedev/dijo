//! The `|SYSTEM` file: the help compiler's version, which decides how `|TOPIC` is laid out and
//! compressed, the help file's title and copyright notice, and the language identifier (LCID)
//! WinHelp 4 files name, whose ANSI code page their text is in.

use super::reader::Reader;
use super::Malformed;
use crate::text::charset::Charset;

const MAGIC: u16 = 0x036C;
/// The first minor version of WinHelp 3.1's layout: older files are WinHelp 3.0's.
const WINHELP_31: u16 = 16;
const TITLE: u16 = 1;
const COPYRIGHT: u16 = 2;
const LCID: u16 = 9;
/// Where an LCID record holds the language identifier.
const LCID_AT: usize = 8;
/// The `|SYSTEM` flags of LZ77-compressed topic blocks of 4 KiB and of 2 KiB.
const LZ77_4K: u16 = 4;
const LZ77_2K: u16 = 8;

/// How the topic file of a help file is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Layout {
    /// WinHelp 3.0's: topic blocks of 2 KiB, uncompressed, a topic link giving the next one by
    /// its distance.
    WinHelp30,
    /// WinHelp 3.1's and later: topic blocks of `block_size` bytes, LZ77-compressed or not, a
    /// topic link giving the next one by its position.
    WinHelp31 { block_size: usize, lz77: bool },
}

/// What `|SYSTEM` says.
pub(super) struct System<'a> {
    pub(super) layout: Layout,
    pub(super) title: &'a [u8],
    pub(super) copyright: &'a [u8],
    lcid: Option<u16>,
}

impl<'a> System<'a> {
    pub(super) fn read(content: &'a [u8]) -> Result<Self, Malformed> {
        let mut header = Reader::new(content, "the |SYSTEM file");
        let magic = header.u16()?;
        if magic != MAGIC {
            return Err(Malformed(format!(
                "the |SYSTEM file starts with {magic:#06x}, not {MAGIC:#06x}"
            )));
        }
        let minor = header.u16()?;
        let _major = header.u16()?;
        let _generated = header.u32()?;
        let flags = header.u16()?;
        if minor < WINHELP_31 {
            return Ok(Self {
                layout: Layout::WinHelp30,
                title: header.string(),
                copyright: &[],
                lcid: None,
            });
        }
        let mut system = Self {
            layout: Layout::WinHelp31 {
                block_size: if flags == LZ77_2K { 0x800 } else { 0x1000 },
                lz77: flags == LZ77_4K || flags == LZ77_2K,
            },
            title: &[],
            copyright: &[],
            lcid: None,
        };
        while header.rest().len() >= 4 {
            let kind = header.u16()?;
            let size = usize::from(header.u16()?);
            let data = header.take(size)?;
            match kind {
                TITLE => system.title = Reader::new(data, "the title").string(),
                COPYRIGHT => system.copyright = Reader::new(data, "the copyright").string(),
                LCID => {
                    let mut fields = Reader::new(data, "the LCID record");
                    fields.skip(LCID_AT)?;
                    system.lcid = Some(fields.u16()?);
                }
                _ => {}
            }
        }
        Ok(system)
    }

    /// The charset of the help file's text: the ANSI code page of its LCID, or Windows-1252,
    /// that of the English WinHelp 3 wrote its files in, where it names none.
    pub(super) fn charset(&self) -> Charset {
        let name = self.lcid.map_or("windows-1252", ansi_code_page);
        Charset::for_name(name)
            .filter(|charset| charset.decodes())
            .unwrap_or_else(|| panic!("the JDK decodes the charset {name} of an LCID"))
    }
}

/// The JDK name of the ANSI code page Windows gives the language identifier `lcid`: by its
/// primary language, Chinese and the languages written in Latin or Cyrillic letters by their
/// sublanguage too.
pub(super) fn ansi_code_page(lcid: u16) -> &'static str {
    let (primary, sublanguage) = (lcid & 0x3FF, lcid >> 10);
    match primary {
        0x04 if matches!(sublanguage, 1 | 3 | 5) => "x-windows-950",
        0x04 => "GBK",
        0x11 => "windows-31j",
        0x12 => "x-windows-949",
        0x1E => "x-windows-874",
        0x1A if matches!(sublanguage, 3 | 7 | 8 | 10 | 12) => "windows-1251",
        0x05 | 0x0E | 0x15 | 0x18 | 0x1A | 0x1B | 0x1C | 0x24 => "windows-1250",
        0x2C | 0x43 if sublanguage == 2 => "windows-1251",
        0x02 | 0x19 | 0x22 | 0x23 | 0x2F | 0x3F | 0x40 | 0x44 | 0x50 | 0x6D => "windows-1251",
        0x08 => "windows-1253",
        0x1F | 0x2C | 0x43 => "windows-1254",
        0x0D | 0x3D => "windows-1255",
        0x01 | 0x20 | 0x29 | 0x8C => "windows-1256",
        0x25..=0x27 => "windows-1257",
        0x2A => "windows-1258",
        _ => "windows-1252",
    }
}
