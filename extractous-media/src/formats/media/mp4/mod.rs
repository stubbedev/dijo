//! MP4, QuickTime and 3GPP files, as Tika's `MP4Parser` reads them through metadata-extractor:
//! the boxes walked ([`walk`]) into the directories metadata-extractor keeps ([`directory`]),
//! with what Tika's own handlers write on the way, the iTunes metadata of a `udta` box
//! ([`user_data`]), the QuickTime metadata of `keys` and `ilst` boxes ([`quick_time`]) and the
//! formats and bit rates of sample entries ([`sample_entries`]), then the directories written as
//! metadata. Every exception metadata-extractor throws is recorded, as Tika records it.

mod directory;
mod location;
mod quick_time;
mod reader;
mod sample_entries;
mod user_data;
mod walk;

use reader::Thrown;

use crate::config::ParserConfigs;
use crate::formats::embedded::{self, ResourceType, Wrapping};
use crate::formats::metadata::TikaMetadata;
use crate::formats::xhtml::Xhtml;
use crate::formats::{Decline, Failure, Footprint, Opened, Parser, Prepared};

/// `MP4`: the prefix of the QuickTime metadata keys, which the file names.
const QUICK_TIME_KEY_PREFIX: &str = "mp4:";

/// What Tika does to its output while reading the boxes, and once it has read them, in order.
#[derive(Debug)]
enum Event<'a> {
    Set(&'static str, String),
    Add(&'static str, String),
    /// `Metadata.add(MP4, key, value)` of a QuickTime metadata key.
    AddPrefixed(String, String),
    Paragraph(String),
    Cover(Cover<'a>),
    /// `EmbeddedDocumentUtil.recordException`.
    Recorded(Thrown),
}

/// A cover art picture handed to the embedded document extractor.
#[derive(Debug)]
struct Cover<'a> {
    picture: &'a [u8],
    resource_type: ResourceType,
    content_type: Option<&'static str>,
}

pub struct Mp4Parser;

impl Parser for Mp4Parser {
    fn name(&self) -> &'static str {
        "extractous::MP4Parser"
    }
    fn tika_name(&self) -> &'static str {
        "org.apache.tika.parser.mp4.MP4Parser"
    }

    fn media_types(&self) -> &'static [&'static str] {
        &[
            "audio/mp4",
            "video/3gpp",
            "video/3gpp2",
            "video/mp4",
            "video/x-m4v",
            "video/quicktime",
            "application/mp4",
            "video/iso.segment",
            "image/x-canon-cr3",
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
        let walked = walk::walk(input).map_err(Decline::Deferred)?;
        let mut events = walked.events;
        directory::metadata(&walked.movie, &walked.tracks, &mut events);
        Ok(Box::new(move |xhtml| write(&events, xhtml)))
    }
}

/// `MP4Parser.parse` replayed: what was written while the boxes were read, then the
/// directories' metadata.
fn write(events: &[Event<'_>], xhtml: &mut Xhtml) -> Result<(), Failure> {
    for event in events {
        match event {
            Event::Set(name, value) => xhtml.metadata_mut().set(name, value.as_str()),
            Event::Add(name, value) => xhtml.metadata_mut().add(name, value.as_str()),
            Event::AddPrefixed(name, value) => {
                xhtml
                    .metadata_mut()
                    .add_prefixed(QUICK_TIME_KEY_PREFIX, name, value.as_str());
            }
            Event::Paragraph(text) => xhtml.element("p", text)?,
            Event::Cover(cover) => {
                let mut metadata = TikaMetadata::new();
                cover.resource_type.set(&mut metadata);
                if let Some(content_type) = cover.content_type {
                    metadata.set("Content-Type", content_type);
                }
                xhtml.parse_embedded_opened(
                    cover.picture,
                    &metadata,
                    Wrapping::PackageEntry,
                    Opened::Alone,
                )?;
            }
            Event::Recorded(thrown) => {
                embedded::record_exception(xhtml.metadata_mut(), &thrown.exception());
            }
        }
    }
    Ok(())
}

#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub const END_OF_DATA: &str = "java.io.EOFException: End of data reached.\n";
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub const WARNING_META: &str = r#"<meta name="tk:exception:warn" content=""#;

/// A native result as Tika's path writes it, without two fixes of #118: Tika reads on from
/// where a failed read left its stream, short of what it consumed, so it records the end
/// of data once more first; and it reads an `ilst` box past its end, into the box after
/// it, which ends the `udta` box before its location.
#[cfg(all(any(test, feature = "testing"), feature = "tika"))]
pub fn as_tika_reads_it(
    text: &mut String,
    metadata: &mut crate::formats::parity::ComparedMetadata,
    tika: &crate::formats::parity::ComparedMetadata,
) {
    let tika_values = |name: &str| {
        tika.iter()
            .find(|(tika_name, _)| tika_name == name)
            .map(|(_, values)| values.as_slice())
    };
    if let (Some(tika_warnings), Some((_, warnings))) = (
        tika_values(crate::formats::embedded::EXCEPTION_WARNING),
        metadata
            .iter_mut()
            .find(|(name, _)| name == crate::formats::embedded::EXCEPTION_WARNING),
    ) {
        if tika_warnings.first().map(String::as_str) == Some(END_OF_DATA)
            && tika_warnings[1..] == warnings[..]
        {
            warnings.insert(0, END_OF_DATA.to_owned());
            if let Some(at) = text.find(WARNING_META) {
                text.insert_str(at, &format!("{WARNING_META}{END_OF_DATA}\" />\n"));
            }
        }
    }
    for name in ["geo:lat", "geo:long", "geo:alt"] {
        if tika_values(name).is_none() {
            metadata.retain(|(native_name, _)| native_name != name);
            let meta = format!(r#"<meta name="{name}" content=""#);
            if let Some(start) = text.find(&meta) {
                let end = start
                    + text[start..]
                        .find('\n')
                        .map_or(text.len() - start, |end| end + 1);
                text.replace_range(start..end, "");
            }
        }
    }
}

#[cfg(all(test, feature = "tika"))]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::formats::parity::{
        assert_same_as_tika_rewriting, conformance_corpus, documents, Scratch,
    };

    const TIKA_NAME: &str = "org.apache.tika.parser.mp4.MP4Parser";

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut boxed = u32::try_from(body.len() + 8)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        boxed.extend_from_slice(kind);
        boxed.extend_from_slice(body);
        boxed
    }

    fn full(kind: &[u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
        boxed(kind, &[&[version, 0, 0, 0][..], body].concat())
    }

    fn be32(values: &[u32]) -> Vec<u8> {
        values
            .iter()
            .flat_map(|value| value.to_be_bytes())
            .collect()
    }

    /// An iTunes `data` atom.
    fn data(value_type: u32, value: &[u8]) -> Vec<u8> {
        boxed(b"data", &[&be32(&[value_type, 0])[..], value].concat())
    }

    fn handler(kind: &[u8; 4], name: &[u8]) -> Vec<u8> {
        full(b"hdlr", 0, &[&[0; 4][..], kind, &[0; 12], name].concat())
    }

    fn movie_header(version: u8, time_scale: u32, duration: i64) -> Vec<u8> {
        let times = if version == 1 {
            [
                &3_400_000_000i64.to_be_bytes()[..],
                &3_400_000_100i64.to_be_bytes(),
                &time_scale.to_be_bytes(),
                &duration.to_be_bytes(),
            ]
            .concat()
        } else {
            be32(&[
                3_400_000_000,
                3_400_000_100,
                time_scale,
                u32::try_from(duration).unwrap(),
            ])
        };
        full(b"mvhd", version, &[&times[..], &[0; 80]].concat())
    }

    fn track(
        media_header: &[u8],
        handler_type: &[u8; 4],
        information: &[u8],
        entry: &[u8],
        samples: u32,
    ) -> Vec<u8> {
        let description = full(b"stsd", 0, &[&be32(&[1])[..], entry].concat());
        let time_to_sample = full(b"stts", 0, &be32(&[1, samples, 1000]));
        let table = boxed(b"stbl", &[description, time_to_sample].concat());
        let information = boxed(b"minf", &[information, &table].concat());
        let media = boxed(
            b"mdia",
            &[
                media_header,
                &handler(handler_type, b"Handler\0"),
                &information,
            ]
            .concat(),
        );
        let edits = boxed(
            b"edts",
            &full(b"elst", 0, &be32(&[1, 500, u32::MAX, 0x10000])),
        );
        boxed(
            b"trak",
            &[full(b"tkhd", 0, &[0; 80]), edits, media].concat(),
        )
    }

    fn media_header(time_scale: u32, duration: u32) -> Vec<u8> {
        full(
            b"mdhd",
            0,
            &[
                &be32(&[0, 0, time_scale, duration])[..],
                &[0x55, 0xC4, 0, 0],
            ]
            .concat(),
        )
    }

    fn sound_track() -> Vec<u8> {
        let esds = full(
            b"esds",
            0,
            &[
                0x03, 0x19, 0, 1, 0, 0x04, 0x11, 0x40, 0x15, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0xF4,
                0x00,
            ],
        );
        let mut fields = vec![0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 16];
        fields.extend_from_slice(&[0, 0, 0, 0, 0xAC, 0x44, 0, 0]);
        let entry = boxed(b"mp4a", &[fields, esds].concat());
        track(
            &media_header(44100, 120_000),
            b"soun",
            &full(b"smhd", 0, &[0; 4]),
            &entry,
            118,
        )
    }

    fn video_track() -> Vec<u8> {
        let mut fields = vec![0; 78];
        fields[7] = 1;
        fields[24..28].copy_from_slice(&[0, 64, 0, 48]);
        fields[42..53].copy_from_slice(b"\x0aAVC Coding");
        let btrt = boxed(b"btrt", &be32(&[0, 90_000, 80_000]));
        let entry = boxed(b"avc1", &[fields, btrt].concat());
        track(
            &media_header(600, 1638),
            b"vide",
            &full(b"vmhd", 0, &[0; 8]),
            &entry,
            27,
        )
    }

    /// A `©xyz` box of an ISO 6709 location.
    fn location(iso6709: &[u8]) -> Vec<u8> {
        let mut xyz = u16::try_from(iso6709.len()).unwrap().to_be_bytes().to_vec();
        xyz.extend_from_slice(&[0x15, 0xC7]);
        xyz.extend_from_slice(iso6709);
        boxed(b"\xA9xyz", &xyz)
    }

    fn user_data() -> Vec<u8> {
        let items = [
            boxed(b"\xA9nam", &data(1, "Sang om havet".as_bytes())),
            boxed(b"\xA9ART", &data(1, "Kyst K\u{F8}r".as_bytes())),
            boxed(b"\xA9too", &data(1, b"Lavf61.7.100")),
            boxed(b"trkn", &data(0, &[0, 0, 0, 3, 0, 12, 0, 0])),
            boxed(b"disk", &data(0, &[0, 0, 0, 1, 0, 2])),
            boxed(b"cpil", &data(21, &[1])),
            boxed(b"desc", &data(1, b"Beskrivelse")),
            boxed(
                b"covr",
                &[data(0, b"first cover"), data(0, b"second cover")].concat(),
            ),
        ]
        .concat();
        let meta = full(
            b"meta",
            0,
            &[
                handler(b"mdir", b"\0"),
                boxed(b"free", &[0; 4]),
                boxed(b"ilst", &items),
            ]
            .concat(),
        );
        boxed(
            b"udta",
            &[location(b"+55.6761+012.5683+010.000/"), meta].concat(),
        )
    }

    fn quick_time_metadata() -> Vec<u8> {
        let key = |name: &[u8]| boxed(b"mdta", name);
        let keys = full(
            b"keys",
            0,
            &[
                &be32(&[3])[..],
                &key(b"com.apple.quicktime.location.ISO6709"),
                &key(b"com.apple.quicktime.camera.focal_length.35mm_equivalent"),
                &key(b"com.apple.quicktime.live-photo.vitality-score"),
            ]
            .concat(),
        );
        let item = |index: u32, value_type: u32, value: &[u8]| {
            let mut item = be32(&[0, index]);
            item.extend_from_slice(&data(value_type, value));
            let size = u32::try_from(item.len()).unwrap().to_be_bytes();
            item[..4].copy_from_slice(&size);
            item
        };
        let items = boxed(
            b"ilst",
            &[
                item(1, 1, b"+12.3456-098.7654+010.500/"),
                item(2, 21, &[0xFF, 0xF3]),
                item(3, 23, &0.75f32.to_be_bytes()),
                item(4, 1, b"no key"),
            ]
            .concat(),
        );
        boxed(b"meta", &[handler(b"mdta", b"\0"), keys, items].concat())
    }

    fn file(brand: &[u8; 4], movie: &[Vec<u8>], tail: &[u8]) -> Vec<u8> {
        let file_type = boxed(
            b"ftyp",
            &[&brand[..], &[0, 0, 2, 0], b"isomiso2mp41"].concat(),
        );
        [
            file_type,
            boxed(b"moov", &movie.concat()),
            boxed(b"mdat", &[0xA5; 64]),
            tail.to_vec(),
        ]
        .concat()
    }

    #[test]
    fn extracts_mp4s_as_tikas_path_does() {
        let scratch = Scratch::new("mp4");
        let audio = file(
            b"M4A ",
            &[movie_header(0, 1000, 2730), sound_track(), user_data()],
            &[0, 0, 0, 4, b'f', b'r', b'e', b'e'],
        );
        let huge = [
            &[0, 0, 0, 1][..],
            b"free",
            &24u64.to_be_bytes(),
            &[0; 8],
            &[0x90, 0, 0, 0],
            b"mdat",
        ]
        .concat();
        let video = file(
            b"isom",
            &[
                movie_header(1, 600, 1638),
                video_track(),
                sound_track(),
                quick_time_metadata(),
                boxed(b"uuid", &[0x11; 24]),
                boxed(b"udta", &location(b"+553015-0123045.5+12/")),
            ],
            &huge,
        );
        let read = [
            scratch.write("audio.m4a", &audio),
            scratch.write("video.mov", &video),
            scratch.write("cut.mp4", &video[..video.len() / 2]),
            scratch.write("nested.mp4", &audio[..audio.len() - 120]),
            scratch.write(
                "segment.m4s",
                &[
                    boxed(b"styp", b"msdh\0\0\0\0msdhmsix"),
                    boxed(b"moov", &movie_header(0, 1000, 2500)),
                ]
                .concat(),
            ),
            scratch.write(
                "canon.cr3",
                &file(b"crx ", &[movie_header(0, 1000, 2500)], &[]),
            ),
        ];
        let deferred = [scratch.write(
            "exif.mp4",
            &file(
                b"mp42",
                &[boxed(b"uuid", &[&READER_UUID[..], b"Exif"].concat())],
                &[],
            ),
        )];
        // The parse of this one never ends on Tika's path (#118).
        let endless = file(b"mp42", &[movie_header(1, 1024, i64::MIN)], &[]);
        let (_, metadata) = crate::Extractor::new()
            .extract_bytes_to_string(&endless)
            .unwrap();
        assert_eq!(metadata["xmpDM:duration"], ["-9007199254740992.0"]);
        let (parsed, left) = documents(&Mp4Parser, &[scratch.directory()]);
        assert_eq!(left.len(), deferred.len(), "{left:?}");
        assert_eq!(parsed.len(), read.len(), "{parsed:?}");
        assert_same_as_tika_rewriting(
            &read.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
            TIKA_NAME,
            as_tika_reads_it,
        );
    }

    /// The UUID of an Exif `uuid` box.
    const READER_UUID: [u8; 16] = [
        0x05, 0x37, 0xcd, 0xab, 0x9d, 0x0c, 0x44, 0x31, 0xa7, 0x2a, 0xfa, 0x56, 0x1f, 0x2a, 0x11,
        0x3e,
    ];

    #[test]
    #[ignore = "needs Tika's test documents in CONFORMANCE_CORPUS"]
    fn extracts_tikas_test_mp4s_as_tikas_path_does() {
        let (parsed, deferred) = documents(&Mp4Parser, &[&conformance_corpus()]);
        assert!(deferred.is_empty(), "{deferred:?}");
        assert_eq!(parsed.len(), 18, "{parsed:?}");
        let paths: Vec<&Path> = parsed.iter().map(PathBuf::as_path).collect();
        assert_same_as_tika_rewriting(&paths, TIKA_NAME, as_tika_reads_it);
    }
}
