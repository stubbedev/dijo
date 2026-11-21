//! Tika's `TikaUserDataBox`: the iTunes metadata in the `ilst` box of a `udta` box's `meta`
//! box, written as metadata and paragraphs, its cover art handed to the embedded document
//! extractor, and the location of a `©xyz` box.

use super::reader::{Bytes, Read, Thrown};
use super::{Cover, Event};
use crate::formats::embedded::ResourceType;
use crate::formats::tika_dates;
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
    items(reader, length - 8, covers, events)
}

/// `processIList`: the records of an `ilst` box whose header is read, `length` bytes of them,
/// each a field and its `data` atom.
fn items<'a>(
    reader: &mut Bytes<'a>,
    length: u64,
    covers: &mut Covers,
    events: &mut Vec<Event<'a>>,
) -> Read<()> {
    let mut read = 0;
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
        b"\xA9day" => {
            if let Some(date) = tika_dates::to_metadata_string_keep_partial(&value) {
                events.push(Event::Set("xmpDM:releaseDate", date));
            }
            (&[], true)
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn described(events: Vec<Event<'_>>) -> Vec<String> {
        events
            .into_iter()
            .map(|event| match event {
                Event::Set(name, value) => format!("{name}={value}"),
                Event::Paragraph(value) => format!("p={value}"),
                other => panic!("an item makes no {other:?}"),
            })
            .collect()
    }

    fn sized(kind: &[u8], body: &[&[u8]]) -> Vec<u8> {
        let length = 8 + body.iter().map(|part| part.len()).sum::<usize>();
        let mut sized = u32::try_from(length).unwrap().to_be_bytes().to_vec();
        sized.extend_from_slice(kind);
        body.iter().for_each(|part| sized.extend_from_slice(part));
        sized
    }

    /// An `ilst` box that ends the `udta` payload is read to its end and no further, where Tika
    /// 4.1.0 counted its records against its length with the header and read past it.
    #[test]
    fn reads_an_ilst_box_to_its_end() {
        let data = sized(b"data", &[&1u32.to_be_bytes(), &[0; 4], b"Title"]);
        let ilst = sized(b"ilst", &[&sized(b"\xA9nam", &[&data])]);
        let handler = sized(b"hdlr", &[&[0; 8], b"mdir", b"appl", &[0; 9]]);
        let payload = sized(b"meta", &[&[0; 4], &handler, &ilst]);
        let mut events = Vec::new();
        let location = read(&payload, &mut Covers::default(), &mut events).unwrap();
        assert_eq!(location, None);
        assert_eq!(described(events), ["dc:title=Title", "p=Title"]);
    }

    /// Tika's audio-only test file, whose `ilst` box ends its `udta` box and which Tika 4.1.0
    /// read past into an `EOFException`, gives the items tika-app 4.2.0 writes, without one.
    #[test]
    #[ignore = "needs Tika's test documents, from conformance/prepare.sh"]
    fn reads_the_items_of_tikas_audio_only_test_file() {
        let file = std::fs::read(dijo_core::testing::conformance_corpus().join(
            "tika-parsers-standard/tika-parsers-standard-modules/tika-parser-audiovideo-module/\
             src/test/resources/test-documents/testMP4AudioOnly.mp4",
        ))
        .unwrap();
        let start = file.windows(4).position(|kind| kind == b"udta").unwrap();
        let size = u32::from_be_bytes(file[start - 4..start].try_into().unwrap());
        let payload = &file[start + 4..start - 4 + usize::try_from(size).unwrap()];
        let mut events = Vec::new();
        read(payload, &mut Covers::default(), &mut events).unwrap();
        let events = described(events);
        for item in [
            "dc:title=Test Title",
            "xmpDM:artist=Test Artist",
            "dc:creator=Test Artist",
            "xmpDM:album=Test Album",
            "xmpDM:genre=Rock",
            "xmpDM:logComment=Test Comment",
            "xmpDM:releaseDate=2008",
            "xmpDM:trackNumber=1",
            "xmp:CreatorTool=Lavf61.7.100",
        ] {
            assert!(
                events.iter().any(|event| event == item),
                "{item} in {events:?}"
            );
        }
    }

    /// An iTunes date is the release date as `TikaDates` keeps it, a year alone too, and is
    /// written as a paragraph whatever it is.
    #[test]
    fn keeps_the_release_date_tika_dates_reads() {
        let events = |value: &str| {
            let mut events = Vec::new();
            item(b"\xA9day", value.to_owned(), &mut events);
            described(events)
        };
        assert_eq!(events("2008"), ["xmpDM:releaseDate=2008", "p=2008"]);
        assert_eq!(
            events("2017-04-26T07:00:00Z"),
            [
                "xmpDM:releaseDate=2017-04-26T07:00:00Z",
                "p=2017-04-26T07:00:00Z"
            ]
        );
        assert_eq!(events("2018 2018"), ["p=2018 2018"]);
    }
}
