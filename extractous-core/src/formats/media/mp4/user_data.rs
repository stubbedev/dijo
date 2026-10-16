//! Tika's `TikaUserDataBox`: the iTunes metadata in the `ilst` box of a `udta` box's `meta`
//! box, written as metadata and paragraphs, its cover art handed to the embedded document
//! extractor, and the location of a `©xyz` box.

use super::reader::{Bytes, Read, Thrown};
use super::{Cover, Event};
use crate::formats::embedded::ResourceType;
use crate::java;

/// The bytes of a `meta` box Tika has read once it has its handler type.
const READ_TO_HANDLER_TYPE: i32 = 16;

/// The cover art pictures Tika has handed on so far, of which the first is the thumbnail.
#[derive(Debug, Default)]
pub(super) struct Covers(usize);

/// `new TikaUserDataBox(...)`: the boxes of a `udta` payload, read up to one that ends them,
/// what they write added to `events`, and the location string of its `©xyz` box; the first
/// `IOException` stops the box without its location.
pub(super) fn read<'a>(
    payload: &'a [u8],
    covers: &mut Covers,
    events: &mut Vec<Event<'a>>,
) -> Read<Option<String>> {
    let mut reader = Bytes::new(payload);
    let mut coordinates = None;
    while reader.position() < payload.len() as u64 {
        let size = u64::from(reader.u32()?);
        if size <= 4 {
            break;
        }
        match reader.take(4)? {
            b"\xA9xyz" => {
                let length = reader.u16()?;
                reader.skip(2)?;
                coordinates = Some(java::utf8(reader.take(u64::from(length))?));
            }
            b"meta" => {
                reader.u32()?;
                let to_list = i64::from(reader.u32()?) - 4;
                let Ok(to_list) = i32::try_from(to_list) else {
                    break;
                };
                if to_list < 0 {
                    break;
                }
                reader.take(4)?;
                reader.u32()?;
                reader.u32()?;
                let handler_type = reader.take(4)?;
                if handler_type == b"mdir" {
                    item_list(&mut reader, to_list, covers, events)?;
                }
            }
            _ => {
                if size < 8 {
                    break;
                }
                reader.skip(size - 8)?;
            }
        }
    }
    Ok(coordinates)
}

/// `parseUserDataBox` of the iTunes handler: the boxes after the handler box up to the `ilst`
/// box, which [`items`] reads.
fn item_list<'a>(
    reader: &mut Bytes<'a>,
    to_list: i32,
    covers: &mut Covers,
    events: &mut Vec<Event<'a>>,
) -> Read<()> {
    let Ok(to_skip) = u64::try_from(to_list - READ_TO_HANDLER_TYPE) else {
        return Ok(());
    };
    reader.skip(to_skip)?;
    let mut length = u64::from(reader.u32()?);
    while reader.take(4)? != b"ilst" {
        if length < 8 || length >= i32::MAX as u64 {
            return Err(Thrown::Malformed(format!(
                "Malformed box length in udta metadata: {length}"
            )));
        }
        reader.skip(length - 8)?;
        length = u64::from(reader.u32()?);
    }
    if length < 8 || length >= i32::MAX as u64 {
        return Err(Thrown::Malformed(format!(
            "Malformed ilst length in udta metadata: {length}"
        )));
    }
    items(reader, length, covers, events)
}

/// `processIList`: the records of an `ilst` box `length` long, header included, each a field and
/// its `data` atom. Tika counts the records against the whole length, so it reads one more
/// record past the box, from the box after it (#118).
fn items<'a>(
    reader: &mut Bytes<'a>,
    length: u64,
    covers: &mut Covers,
    events: &mut Vec<Event<'a>>,
) -> Read<()> {
    let mut read = 8;
    while read < length {
        let record_start = reader.position();
        let record_length = u64::from(reader.u32()?);
        if record_length < 16 {
            return Ok(());
        }
        let field = reader.take(4)?;
        let field_length = reader.u32()?;
        let atom = reader.take(4)?;
        let record_end = record_start + record_length;
        if atom == b"data" {
            let value_type = reader.u32()? & 0xFF_FFFF;
            reader.skip(4)?;
            let value_length = (field_length as i32).wrapping_sub(16);
            if value_length > 0 {
                let value_length = u64::from(value_length.unsigned_abs());
                let fits = |reader: &Bytes<'_>| reader.position() + value_length <= record_end;
                match field {
                    b"covr" => {
                        if fits(reader) {
                            cover(reader.take(value_length)?, value_type, covers, events);
                        }
                        while reader.position() + 16 <= record_end {
                            let extra_length = reader.u32()?;
                            let extra_atom = reader.take(4)?;
                            let extra_type = reader.u32()? & 0xFF_FFFF;
                            reader.skip(4)?;
                            let extra_value_length = (extra_length as i32).wrapping_sub(16);
                            if extra_atom != b"data"
                                || extra_value_length <= 0
                                || reader.position() + u64::from(extra_value_length.unsigned_abs())
                                    > record_end
                            {
                                break;
                            }
                            let picture =
                                reader.take(u64::from(extra_value_length.unsigned_abs()))?;
                            cover(picture, extra_type, covers, events);
                        }
                    }
                    b"cpil" => {
                        events.push(Event::Set("xmpDM:compilation", reader.i8()?.to_string()))
                    }
                    b"trkn" => {
                        if value_length >= 8 {
                            let track = reader.u32()?;
                            let total = reader.u32()?;
                            events
                                .push(Event::Set("xmpDM:trackNumber", (track as i32).to_string()));
                            let count = total >> 16;
                            if count > 0 {
                                events.push(Event::Set("audio:track-count", count.to_string()));
                            }
                        }
                    }
                    b"disk" => {
                        if value_length >= 6 {
                            let disc = reader.i32()?;
                            let count = reader.i16()?;
                            events.push(Event::Set("xmpDM:discNumber", disc.to_string()));
                            if count > 0 {
                                events.push(Event::Set("audio:disc-count", count.to_string()));
                            }
                        }
                    }
                    _ => {
                        if fits(reader) {
                            let value = java::utf8(reader.take(value_length)?);
                            item(field, value, events);
                        }
                    }
                }
            }
        }
        let position = reader.position();
        if position > record_end {
            return Ok(());
        }
        reader.skip(record_end - position)?;
        read += record_length;
    }
    Ok(())
}

/// `handleCoverArt`: a picture handed to the embedded document extractor, the first as the
/// thumbnail, with the image type its `data` atom's type names.
fn cover<'a>(picture: &'a [u8], value_type: u32, covers: &mut Covers, events: &mut Vec<Event<'a>>) {
    let resource_type = if covers.0 == 0 {
        ResourceType::Thumbnail
    } else {
        ResourceType::Inline
    };
    covers.0 += 1;
    let content_type = match value_type {
        13 => Some("image/jpeg"),
        14 => Some("image/png"),
        27 => Some("image/bmp"),
        _ => None,
    };
    events.push(Event::Cover(Cover {
        picture,
        resource_type,
        content_type,
    }));
}

/// `addMetadata`: the metadata names an iTunes field is written under, and whether it is also
/// written as a paragraph.
fn item(field: &[u8], value: String, events: &mut Vec<Event<'_>>) {
    let (names, paragraph): (&[&'static str], bool) = match field {
        b"\xA9nam" => (&["dc:title"], true),
        b"\xA9too" => (&["xmp:CreatorTool"], false),
        b"\xA9ART" => (&["xmpDM:artist", "dc:creator"], true),
        b"aART" => (&["xmpDM:albumArtist"], true),
        b"\xA9wrt" => (&["xmpDM:composer"], true),
        b"\xA9alb" => (&["xmpDM:album"], true),
        b"\xA9gen" => (&["xmpDM:genre"], true),
        b"\xA9day" => (&["xmpDM:releaseDate"], true),
        b"\xA9cmt" => (&["xmpDM:logComment"], true),
        b"cprt" => (&["xmpDM:copyright"], true),
        b"keyw" => (&["dc:subject"], true),
        b"\xA9lyr" => (&[], true),
        b"ldes" | b"desc" => (&["dc:description"], true),
        b"xid " => (&["xmp:Identifier"], false),
        _ => return,
    };
    for name in names {
        events.push(Event::Set(name, value.clone()));
    }
    if paragraph {
        events.push(Event::Paragraph(value));
    }
}
