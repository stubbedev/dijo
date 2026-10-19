use super::*;
use crate::config::TesseractOcrConfig;

/// With OCR skipped, the OCR enricher is not recorded among the parsers, where Tika
/// records it though it reads nothing (#114).
#[test]
fn records_no_ocr_parser_where_ocr_is_skipped() {
    let (_, metadata) = crate::Extractor::new()
        .set_ocr_config(TesseractOcrConfig::new().set_skip_ocr(true))
        .extract_file_to_string("../test_files/documents/ara-ocr.png")
        .unwrap();
    assert_eq!(metadata["tk:parsed-by"], ["extractous::ImageParser"]);
    assert!(!metadata["tk:parsed-by-full-set"]
        .iter()
        .any(|parser| parser == PREPARING_OCR_PARSER));
}

/// A HEIC image libheif cannot decode for OCR fails as the `IOException` our Java
/// `HeifDecoder` throws out of Tika's `HeifParser` (#173).
#[test]
fn fails_on_heif_images_libheif_cannot_decode() {
    let heic = std::fs::read("../quality/corpus/prose-scan.heic").unwrap();
    let mut metadata = TikaMetadata::new();
    metadata.set("Content-Type", "image/heic");
    let configs = ParserConfigs::default();
    let prepared = HeifParser.prepare(&heic[..heic.len() / 2], &metadata, &configs);
    match prepared {
        Err(Decline::Fails(Error::ParseError(message))) => assert!(
            message.starts_with(
                "Parse error occurred : org.apache.tika.exception.TikaException: TIKA-198: \
                 Illegal IOException from org.apache.tika.parser.image.HeifParser <- caused by \
                 java.io.IOException: "
            ),
            "{message}"
        ),
        Ok(_) => panic!("a truncated HEIC image is read"),
        Err(other) => panic!("{other:?}"),
    };
}

/// What Tika's `ImageParser` reads of an image through the JDK's own readers, as
/// `BmpReference.java` records it: its metadata, sorted by name, or the message at the root of
/// its failure.
#[derive(Debug, PartialEq, Eq)]
enum JdkRead {
    Metadata(Vec<(String, String)>),
    Fails(String),
}

/// The images of `bmp-reference.tsv`, by name, with what the JDK's readers read of them.
fn jdk_reads() -> Vec<(String, JdkRead)> {
    let unescape = |text: &str| {
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            out.push(match chars.next() {
                Some('t') => '\t',
                Some('n') => '\n',
                Some('r') => '\r',
                Some(other) => other,
                None => panic!("a line of bmp-reference.tsv ends in a backslash"),
            });
        }
        out
    };
    let mut reads: Vec<(String, JdkRead)> = Vec::new();
    for line in include_str!("../../../tests/fixtures/image/bmp-reference.tsv").lines() {
        if let Some(name) = line.strip_prefix('#') {
            reads.push((name.to_owned(), JdkRead::Metadata(Vec::new())));
            continue;
        }
        let (_, read) = reads.last_mut().expect("a #name line first");
        match line.strip_prefix('!') {
            Some(message) => *read = JdkRead::Fails(unescape(message)),
            None => {
                let (name, values) = line.split_once('\t').expect("a name and its values");
                let JdkRead::Metadata(metadata) = read else {
                    panic!("metadata after a failure: {line}");
                };
                metadata.push((unescape(name), unescape(values)));
            }
        }
    }
    reads
}

/// What the native reader reads of the image `input` named `name`, as `BmpReference.java`
/// records what the JDK's readers read.
fn native_read(name: &str, input: &[u8]) -> JdkRead {
    let mut metadata = TikaMetadata::new();
    let content_type = if name.ends_with(".wbmp") {
        "image/vnd.wap.wbmp"
    } else {
        "image/bmp"
    };
    metadata.set("Content-Type", content_type);
    match image_io(input, metadata) {
        Ok(read) => {
            let mut names: Vec<&str> = read.metadata.names().collect();
            names.sort_unstable();
            JdkRead::Metadata(
                names
                    .into_iter()
                    .map(|name| (name.to_owned(), read.metadata.values(name).join("|")))
                    .collect(),
            )
        }
        Err(Decline::Fails(error)) => JdkRead::Fails(error.to_string()),
        Err(other) => panic!("{name} is not read: {other:?}"),
    }
}

/// Asserts that the native reader reads `input`, named `name`, as the JDK's do: the same
/// metadata, or a failure whose message holds that of the JDK's failure, where it has one.
fn assert_read_as_the_jdk_does(name: &str, input: &[u8], expected: &JdkRead) {
    match (native_read(name, input), expected) {
        (JdkRead::Fails(native), JdkRead::Fails(jdk)) => assert!(
            jdk == "null" || native.contains(jdk.as_str()),
            "{name}: the native reader fails with {native:?}, the JDK's with {jdk:?}"
        ),
        (native, expected) => assert_eq!(&native, expected, "{name}"),
    }
}

/// Tika reads BMP and WBMP images with the JDK's readers or jai-imageio's, whichever its
/// registry lists first in an order nothing defines (#114); the native readers read them as the
/// JDK's do, the readers the Java path has without jai-imageio, as `just bmp-reference`
/// records them.
#[test]
fn reads_bmps_and_wbmps_as_the_jdks_readers_do() {
    let mut read = 0;
    for (name, expected) in jdk_reads() {
        if name == TIKAS_TEST_BMP {
            continue;
        }
        let input = std::fs::read(format!(
            "../extractous-image/tests/fixtures/image/bmp/{name}"
        ))
        .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_read_as_the_jdk_does(&name, &input, &expected);
        read += 1;
    }
    assert_eq!(read, 27);
}

/// An ISO base media file box of `kind` around `payload`.
fn bmff(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    [
        &((payload.len() + 8) as u32).to_be_bytes()[..],
        kind,
        payload,
    ]
    .concat()
}

/// A full box: a box whose payload starts with a version and three bytes of flags.
fn full_bmff(kind: &[u8; 4], version: u8, payload: &[u8]) -> Vec<u8> {
    bmff(kind, &[&[version, 0, 0, 0][..], payload].concat())
}

/// A big-endian IFD entry of `tag`, `format`, `count` and a value or offset of four bytes.
fn ifd_entry(tag: u16, format: u16, count: u32, value: [u8; 4]) -> Vec<u8> {
    [
        &tag.to_be_bytes()[..],
        &format.to_be_bytes(),
        &count.to_be_bytes(),
        &value,
    ]
    .concat()
}

/// A big-endian IFD of these entries, with no IFD after it.
fn ifd(entries: &[Vec<u8>]) -> Vec<u8> {
    [
        &(entries.len() as u16).to_be_bytes()[..],
        &entries.concat(),
        &[0; 4],
    ]
    .concat()
}

/// The binary property list of an Apple run time: valid, with these seconds at a nanosecond
/// scale.
fn apple_run_time(seconds: i64) -> Vec<u8> {
    let objects: Vec<Vec<u8>> = vec![
        vec![0xD4, 1, 2, 3, 4, 5, 6, 7, 8],
        [&[0x55][..], b"flags"].concat(),
        [&[0x55][..], b"value"].concat(),
        [&[0x59][..], b"timescale"].concat(),
        [&[0x55][..], b"epoch"].concat(),
        vec![0x10, 1],
        [&[0x13][..], &(seconds * 1_000_000_000).to_be_bytes()].concat(),
        [&[0x12][..], &1_000_000_000u32.to_be_bytes()].concat(),
        vec![0x10, 0],
    ];
    let mut plist = b"bplist00".to_vec();
    let mut offsets = Vec::new();
    for object in &objects {
        offsets.push(plist.len() as u8);
        plist.extend_from_slice(object);
    }
    let table = plist.len() as i64;
    plist.extend_from_slice(&offsets);
    plist.extend_from_slice(&[0; 6]);
    plist.extend_from_slice(&[1, 1]);
    plist.extend_from_slice(&(objects.len() as i64).to_be_bytes());
    plist.extend_from_slice(&0i64.to_be_bytes());
    plist.extend_from_slice(&table.to_be_bytes());
    plist
}

/// An Apple maker note: its header, then an IFD of its run time, acceleration vector, HDR image
/// type and live photo id, whose offsets count from the note's start.
fn apple_maker_note() -> Vec<u8> {
    const HEADER: usize = 14;
    const ENTRIES: u16 = 4;
    let data_start = HEADER + 2 + 12 * usize::from(ENTRIES) + 4;
    let run_time = apple_run_time(595_778);
    let acceleration: Vec<u8> = [(-93i32, 100i32), (1, 100), (35, -100)]
        .iter()
        .flat_map(|(numerator, denominator)| {
            [numerator.to_be_bytes(), denominator.to_be_bytes()].concat()
        })
        .collect();
    let acceleration_at = data_start + run_time.len();
    let note = ifd(&[
        ifd_entry(
            0x0003,
            7,
            run_time.len() as u32,
            (data_start as u32).to_be_bytes(),
        ),
        ifd_entry(0x0008, 10, 3, (acceleration_at as u32).to_be_bytes()),
        ifd_entry(0x000a, 9, 1, 3i32.to_be_bytes()),
        ifd_entry(0x0017, 4, 1, 8192u32.to_be_bytes()),
    ]);
    [&b"Apple iOS\0\0\x01MM"[..], &note, &run_time, &acceleration].concat()
}

/// Big-endian TIFF data of an iPhone's Exif: IFD0's make and model, and the Exif IFD with its
/// maker note.
fn iphone_exif() -> Vec<u8> {
    let maker_note = apple_maker_note();
    let ifd0_entries = 4u32;
    let ifd0_length = 2 + 12 * ifd0_entries + 4;
    let strings_at = 8 + ifd0_length;
    let exif_ifd_at = strings_at + 16;
    let exif_ifd_length = 2 + 12 + 4;
    let note_at = exif_ifd_at + exif_ifd_length;
    let ifd0 = ifd(&[
        ifd_entry(0x010f, 2, 6, strings_at.to_be_bytes()),
        ifd_entry(0x0110, 2, 9, (strings_at + 6).to_be_bytes()),
        ifd_entry(0x0112, 3, 1, [0, 1, 0, 0]),
        ifd_entry(0x8769, 4, 1, exif_ifd_at.to_be_bytes()),
    ]);
    let exif_ifd = ifd(&[ifd_entry(
        0x927c,
        7,
        maker_note.len() as u32,
        note_at.to_be_bytes(),
    )]);
    [
        &b"MM\0\x2a\0\0\0\x08"[..],
        &ifd0,
        b"Apple\0iPhone 8\0\0",
        &exif_ifd,
        &maker_note,
    ]
    .concat()
}

/// A HEIF image as [`iphone_heic`], or with `exif_first` the Exif item in a free box before
/// the meta box.
pub(super) fn heic(exif_first: bool) -> Vec<u8> {
    let file_type = bmff(b"ftyp", &[&b"heic"[..], &[0; 4], b"mif1heic"].concat());
    let exif = [&6u32.to_be_bytes()[..], b"Exif\0\0", &iphone_exif()].concat();
    let image = b"not an image".to_vec();
    let meta = |image_at: u32, exif_at: u32| {
        let info = full_bmff(
            b"iinf",
            0,
            &[
                &2u16.to_be_bytes()[..],
                &full_bmff(b"infe", 2, &[&[0, 1, 0, 0][..], b"hvc1\0"].concat()),
                &full_bmff(b"infe", 2, &[&[0, 2, 0, 0][..], b"Exif\0"].concat()),
            ]
            .concat(),
        );
        let location = |item: u16, at: u32, length: usize| {
            [
                &item.to_be_bytes()[..],
                &[0, 0, 0, 1],
                &at.to_be_bytes(),
                &(length as u32).to_be_bytes(),
            ]
            .concat()
        };
        let locations = full_bmff(
            b"iloc",
            0,
            &[
                &[0x44, 0x00, 0, 2][..],
                &location(1, image_at, image.len()),
                &location(2, exif_at, exif.len()),
            ]
            .concat(),
        );
        let properties = bmff(
            b"iprp",
            &bmff(
                b"ipco",
                &[
                    full_bmff(
                        b"ispe",
                        0,
                        &[&512u32.to_be_bytes()[..], &384u32.to_be_bytes()].concat(),
                    ),
                    full_bmff(b"pixi", 0, &[3, 8, 8, 8]),
                    bmff(b"irot", &[1]),
                    bmff(
                        b"colr",
                        &[&b"nclx"[..], &[0, 1, 0, 13, 0, 6, 0x80]].concat(),
                    ),
                ]
                .concat(),
            ),
        );
        full_bmff(
            b"meta",
            0,
            &[
                full_bmff(
                    b"hdlr",
                    0,
                    &[&[0; 4][..], b"pict", &[0; 12], b"\0"].concat(),
                ),
                full_bmff(b"pitm", 0, &1u16.to_be_bytes()),
                info,
                locations,
                properties,
            ]
            .concat(),
        )
    };
    if exif_first {
        let free = bmff(b"free", &exif);
        let exif_at = (file_type.len() + 8) as u32;
        let media_at = (file_type.len() + free.len() + meta(0, 0).len() + 8) as u32;
        return [
            file_type,
            free,
            meta(media_at, exif_at),
            bmff(b"mdat", &image),
        ]
        .concat();
    }
    let media_at = (file_type.len() + meta(0, 0).len() + 8) as u32;
    let exif_at = media_at + image.len() as u32;
    [
        file_type,
        meta(media_at, exif_at),
        bmff(b"mdat", &[&image[..], &exif].concat()),
    ]
    .concat()
}

#[test]
fn reads_the_exif_item_of_a_heif_image_wherever_it_stands() {
    // The Exif item before the media data, where metadata-extractor reads the image as the
    // Exif (#114).
    let swapped = heic(true);
    let (_, metadata) = crate::Extractor::new()
        .set_ocr_config(TesseractOcrConfig::new().set_skip_ocr(true))
        .extract_bytes_to_string(&swapped)
        .unwrap();
    assert_eq!(
        metadata.get("tiff:Model").map(Vec::as_slice),
        Some(&["iPhone 8".to_owned()][..]),
        "{metadata:?}"
    );
}

#[test]
fn describes_an_image_read_from_memory_by_its_size_alone() {
    let input = std::fs::read("../quality/corpus/prose-scan.webp").unwrap();
    let (_, metadata) = crate::Extractor::new()
        .extract_bytes_to_string(&input)
        .unwrap();
    // Tika describes the temporary file it spools the image to (#114).
    assert_eq!(
        metadata.get("img:File Size").map(Vec::as_slice),
        Some(&[format!("{} bytes", input.len())][..]),
        "{metadata:?}"
    );
    assert!(!metadata.contains_key("img:File Name"), "{metadata:?}");
    assert!(
        !metadata.contains_key("img:File Modified Date"),
        "{metadata:?}"
    );
}

/// The fixture of a JPEG XL image named `name`.
pub(super) fn jxl_fixture(name: &str) -> String {
    format!("{}/tests/fixtures/jxl/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn reads_the_header_and_boxes_of_jpeg_xl_images() {
    let (xhtml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .set_ocr_config(TesseractOcrConfig::new().set_skip_ocr(true))
        .extract_file_to_string(&jxl_fixture("card.jxl"))
        .unwrap();
    let value = |name: &str| metadata.get(name).map(|values| values.join("|"));
    for (name, expected) in [
        ("Content-Type", "image/jxl"),
        ("tiff:ImageWidth", "96"),
        ("tiff:ImageLength", "80"),
        ("tiff:BitsPerSample", "8"),
        ("tiff:Make", "Extractous"),
        ("tiff:Orientation", "6"),
        ("dc:title", "Fyret"),
        ("tk:parsed-by", "extractous::JXLParser"),
    ] {
        assert_eq!(
            value(name).as_deref(),
            Some(expected),
            "{name}: {metadata:?}"
        );
    }
    assert!(
        xhtml.contains("<body />") || xhtml.contains("<body>"),
        "{xhtml}"
    );
}

#[test]
fn reads_jpeg_xl_scans_as_the_same_png() {
    assert!(crate::ocr::tesseract_runs(), "tesseract is not installed");
    let text = |path: &str| {
        crate::Extractor::new()
            .extract_file_to_string(path)
            .unwrap()
            .0
    };
    let png = text(&format!(
        "{}/../quality/corpus/danish-scan.png",
        env!("CARGO_MANIFEST_DIR")
    ));
    assert!(png.contains("fyr"), "{png}");
    assert_eq!(text(&jxl_fixture("scan.jxl")), png);
}
