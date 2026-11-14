use super::media::tests::grey_jpeg;
use super::*;

/// Bit fields packed most significant bit first, as SWF packs them.
#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    byte: u8,
    count: u32,
}

impl Bits {
    fn put(&mut self, value: i64, bits: u32) {
        for shift in (0..bits).rev() {
            self.byte = self.byte << 1 | u8::from(value >> shift & 1 == 1);
            self.count += 1;
            if self.count == 8 {
                self.out.push(self.byte);
                (self.byte, self.count) = (0, 0);
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            self.out.push(self.byte << (8 - self.count));
        }
        self.out
    }
}

fn tag(code: u16, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    match u16::try_from(body.len())
        .ok()
        .filter(|&length| length < 0x3F)
    {
        Some(length) => out.extend((code << 6 | length).to_le_bytes()),
        None => {
            out.extend((code << 6 | 0x3F).to_le_bytes());
            out.extend(u32::try_from(body.len()).unwrap().to_le_bytes());
        }
    }
    out.extend(body);
    out
}

/// A movie's body of `tags`, at 24 frames a second, of one frame, ended.
fn body(tags: &[Vec<u8>]) -> Vec<u8> {
    [
        vec![0],
        (24u16 << 8).to_le_bytes().to_vec(),
        1u16.to_le_bytes().to_vec(),
        tags.concat(),
        tag(END, &[]),
    ]
    .concat()
}

/// A movie of `version` whose body is `body`, compressed as `signature` says.
fn movie_of(signature: &[u8; 3], version: u8, body: &[u8]) -> Vec<u8> {
    let length = u32::try_from(body.len() + HEADER_BYTES)
        .unwrap()
        .to_le_bytes();
    let packed = match signature {
        b"FWS" => body.to_vec(),
        b"CWS" => crate::compress::zlib::tests::zlib(body),
        b"ZWS" => {
            let alone = crate::compress::xz::tests::lzma(body);
            let data = &alone[13..];
            let compressed = u32::try_from(data.len()).unwrap().to_le_bytes();
            [&compressed[..], &alone[..5], data].concat()
        }
        _ => panic!("no signature {signature:?}"),
    };
    [&signature[..], &[version], &length, &packed[..]].concat()
}

fn movie(version: u8, tags: &[Vec<u8>]) -> Vec<u8> {
    movie_of(b"FWS", version, &body(tags))
}

/// The printable ASCII characters, glyph `c - 0x20` of the fonts the tests define.
fn ascii() -> String {
    (0x20u8..0x7F).map(char::from).collect()
}

/// A `DefineFont2` or `DefineFont3` of empty glyphs for `characters`, its codes two bytes.
fn font_2(code: u16, id: u16, characters: &str) -> Vec<u8> {
    let count = characters.chars().count();
    let table = 2 * count + 2;
    let mut body = [&id.to_le_bytes()[..], &[0x04, 0, 5], b"Arial"].concat();
    body.extend(u16::try_from(count).unwrap().to_le_bytes());
    for glyph in 0..count {
        body.extend(u16::try_from(table + 2 * glyph).unwrap().to_le_bytes());
    }
    body.extend(u16::try_from(table + 2 * count).unwrap().to_le_bytes());
    body.extend([0x10, 0x00].repeat(count));
    for character in characters.chars() {
        body.extend(u16::try_from(u32::from(character)).unwrap().to_le_bytes());
    }
    tag(code, &body)
}

/// A run of glyphs of a text record: its font and height, X and Y offsets, if it sets them,
/// and its glyph indexes, each advancing 100 twips.
struct Run<'a> {
    font: Option<(u16, u16)>,
    x: Option<i16>,
    y: Option<i16>,
    glyphs: &'a [u32],
}

const ADVANCE: i64 = 100;

/// A `DefineText` or `DefineText2` of `runs`, its glyph entries of 7-bit glyphs and 11-bit
/// advances.
fn text(code: u16, id: u16, runs: &[Run]) -> Vec<u8> {
    let mut body = [&id.to_le_bytes()[..], &[0x00, 0x00, 7, 11]].concat();
    for run in runs {
        let flags = 0x80
            | u8::from(run.font.is_some()) << 3
            | u8::from(run.y.is_some()) << 1
            | u8::from(run.x.is_some());
        body.push(flags);
        if let Some((font, _)) = run.font {
            body.extend(font.to_le_bytes());
        }
        if let Some(x) = run.x {
            body.extend(x.to_le_bytes());
        }
        if let Some(y) = run.y {
            body.extend(y.to_le_bytes());
        }
        if let Some((_, height)) = run.font {
            body.extend(height.to_le_bytes());
        }
        body.push(u8::try_from(run.glyphs.len()).unwrap());
        let mut entries = Bits::default();
        for &glyph in run.glyphs {
            entries.put(i64::from(glyph), 7);
            entries.put(ADVANCE, 11);
        }
        body.extend(entries.finish());
    }
    body.push(0);
    tag(code, &body)
}

/// The glyph indexes of `text` in a font of [`ascii`].
fn glyphs(text: &str) -> Vec<u32> {
    text.bytes().map(|byte| u32::from(byte - 0x20)).collect()
}

/// A `DefineEditText` without a font or layout, of the initial `text` and the `flags` and
/// `more` flags besides `HasText`.
fn edit_text(id: u16, flags: u8, more: u8, text: &[u8]) -> Vec<u8> {
    let body = [
        &id.to_le_bytes()[..],
        &[0x00, EDIT_FLAG_HAS_TEXT | flags, more, 0],
        text,
        &[0],
    ]
    .concat();
    tag(DEFINE_EDIT_TEXT, &body)
}

const EDIT_FLAG_HAS_TEXT: u8 = 0x80;
const EDIT_FLAG_PASSWORD: u8 = 0x10;
const EDIT_FLAG_HTML: u8 = 0x02;

/// The XHTML body, without its line breaks and indents, and the metadata of `file`.
fn extract(file: &[u8]) -> (String, dijo_core::Metadata) {
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(file)
        .unwrap();
    assert_eq!(metadata["Content-Type"], ["application/x-shockwave-flash"]);
    assert_eq!(metadata["tk:parsed-by"].last().unwrap(), NAME);
    let body = crate::formats::testing::body(&xml).to_owned();
    (body.replace(['\n', '\t'], ""), metadata)
}

fn failure(file: &[u8]) -> String {
    crate::Extractor::new()
        .extract_bytes_to_string(file)
        .unwrap_err()
        .to_string()
}

fn static_text() -> Vec<Vec<u8>> {
    vec![
        font_2(DEFINE_FONT_3, 1, &ascii()),
        text(
            DEFINE_TEXT,
            10,
            &[
                Run {
                    font: Some((1, 480)),
                    x: Some(0),
                    y: Some(400),
                    glyphs: &glyphs("Skagen Lighthouse"),
                },
                Run {
                    font: Some((1, 240)),
                    x: Some(0),
                    y: Some(900),
                    glyphs: &glyphs("Lit every evening"),
                },
                Run {
                    font: Some((1, 240)),
                    x: None,
                    y: None,
                    glyphs: &glyphs(" at dusk"),
                },
                Run {
                    font: None,
                    x: Some(0),
                    y: Some(1300),
                    glyphs: &glyphs("Tide"),
                },
                Run {
                    font: None,
                    x: Some(i16::try_from(4 * ADVANCE + 200).unwrap()),
                    y: None,
                    glyphs: &glyphs("06:12"),
                },
                Run {
                    font: None,
                    x: Some(i16::try_from(9 * ADVANCE + 10).unwrap()),
                    y: Some(1300),
                    glyphs: &glyphs("!"),
                },
            ],
        ),
    ]
}

const STATIC_TEXT: &str =
    "<p>Skagen Lighthouse<br />Lit every evening at dusk<br />Tide 06:12!</p>";

#[test]
fn writes_static_text_a_line_to_each_y_offset_and_a_space_across_gaps() {
    let (body, metadata) = extract(&movie(10, &static_text()));
    assert_eq!(body, STATIC_TEXT);
    assert!(!metadata.contains_key(EXCEPTION_WARNING));
}

#[test]
fn reads_compressed_movies_as_uncompressed_ones() {
    let body = body(&static_text());
    for signature in [b"FWS", b"CWS", b"ZWS"] {
        let (text, metadata) = extract(&movie_of(signature, 13, &body));
        assert_eq!(text, STATIC_TEXT, "{signature:?}");
        assert_eq!(metadata["swf:version"], ["13"], "{signature:?}");
    }
}

#[test]
fn decodes_compressed_bodies_up_to_the_length_the_header_declares() {
    let body = body(&static_text());
    for signature in [b"CWS", b"ZWS"] {
        let mut longer = body.clone();
        longer.extend([0; 100_000]);
        let mut file = movie_of(signature, 10, &longer);
        let declared = u32::try_from(body.len() + HEADER_BYTES).unwrap();
        file[4..8].copy_from_slice(&declared.to_le_bytes());
        let header = Header::read(&file).unwrap();
        assert_eq!(header.length, declared);
        let decoded = header
            .body(&file)
            .unwrap_or_else(|_| panic!("{signature:?}"));
        assert_eq!(&*decoded, &body[..], "{signature:?}");
        let (text, _) = extract(&file);
        assert_eq!(text, STATIC_TEXT, "{signature:?}");
    }
    let mut longer = movie_of(b"FWS", 10, &body);
    longer.extend(b"trailing bytes past the declared length");
    let header = Header::read(&longer).unwrap();
    assert_eq!(&*header.body(&longer).unwrap(), &body[..]);
}

#[test]
fn reads_the_header_of_each_compression() {
    let fws = movie_of(b"FWS", 8, &body(&[]));
    assert_eq!(
        Header::read(&fws),
        Ok(Header {
            compression: Compression::None,
            version: 8,
            length: u32::try_from(fws.len()).unwrap(),
        })
    );
    let zws = movie_of(b"ZWS", 13, &body(&[]));
    assert!(matches!(
        Header::read(&zws).unwrap().compression,
        Compression::Lzma { .. }
    ));
    for (file, reason) in [
        (&b"FWS\x0a\x07\0\0\0"[..], "shorter than its header"),
        (&b"FWS\x0a"[..], "ends within its 8-byte header"),
        (&b"ZWS\x0d\x20\0\0\0\x10\0\0\0\x5d"[..], "LZMA properties"),
        (&b"GWS\x0a\x20\0\0\0"[..], "not FWS, CWS or ZWS"),
    ] {
        let error = Header::read(file).unwrap_err();
        assert!(error.contains(reason), "{error}");
    }
}

#[test]
fn maps_glyphs_through_font_info_code_tables_in_the_charset_of_older_movies() {
    let define_font = tag(DEFINE_FONT, &[3, 0, 4, 0, 6, 0, 0x10, 0, 0x10, 0]);
    let font_info = tag(
        DEFINE_FONT_INFO,
        &[[3, 0, 5].as_slice(), b"Times", &[0x08, 0x80, b'a']].concat(),
    );
    let define_font_4 = tag(DEFINE_FONT, &[4, 0, 2, 0, 0x10, 0]);
    let shift_jis = tag(
        DEFINE_FONT_INFO_2,
        &[[4, 0, 0].as_slice(), &[0x11, 2], &0x82A0u16.to_le_bytes()].concat(),
    );
    let run = |font: u16, glyphs: &'static [u32]| Run {
        font: Some((font, 240)),
        x: Some(0),
        y: Some(100),
        glyphs,
    };
    let tags = [
        define_font,
        font_info,
        define_font_4,
        shift_jis,
        font_2(DEFINE_FONT_2, 5, "\u{f8}"),
        text(DEFINE_TEXT, 10, &[run(3, &[0, 1])]),
        text(DEFINE_TEXT, 11, &[run(4, &[0])]),
        text(DEFINE_TEXT, 12, &[run(5, &[0])]),
        edit_text(13, 0, 0, b"\x93Lamp\x94"),
    ];
    let (body, _) = extract(&movie(5, &tags));
    assert_eq!(
        body,
        "<p>\u{20ac}a</p><p>\u{3042}</p><p>\u{f8}</p><p>\u{201c}Lamp\u{201d}</p>"
    );
}

#[test]
fn writes_text_fields_of_html_as_paragraphs_and_lists_and_leaves_passwords_out() {
    let html = "<p align=\"left\"><font face=\"Arial\" size=\"14\">Keeper's log</font></p>\
                <p>Oil on <b>Mondays</b>.<br>Wicks <a href=\"http://example.org/\">trimmed\
                </a>.</p><li>Lamp</li><li>Lens</li>after<script>left out</script>";
    let tags = [
        edit_text(12, 0, EDIT_FLAG_HTML, html.as_bytes()),
        edit_text(
            13,
            0,
            0,
            "Visitors welcome\r\nClosed on Sundays\r".as_bytes(),
        ),
        edit_text(14, 0, 0, "N\u{f8}rre Kaj\rQuay\n\nStreet".as_bytes()),
        edit_text(15, EDIT_FLAG_PASSWORD, 0, b"lamp-room-key"),
        edit_text(16, 0, EDIT_FLAG_HTML, b"<p></p>"),
    ];
    let (body, _) = extract(&movie(10, &tags));
    assert_eq!(
        body,
        "<p>Keeper's log</p><p>Oil on Mondays.<br />Wicks trimmed.</p>\
         <ul><li>Lamp</li><li>Lens</li></ul><p>after</p>\
         <p>Visitors welcome<br />Closed on Sundays</p>\
         <p>N\u{f8}rre Kaj<br />Quay<br />Street</p>"
    );
}

#[test]
fn reads_the_header_file_attributes_and_xmp_into_the_metadata() {
    let xmp = "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF \
               xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description \
               rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\"><dc:title><rdf:Alt>\
               <rdf:li xml:lang=\"x-default\">Lighthouse</rdf:li></rdf:Alt></dc:title>\
               <dc:creator><rdf:Seq><rdf:li>Anna Berg</rdf:li></rdf:Seq></dc:creator>\
               </rdf:Description></rdf:RDF></x:xmpmeta>\0";
    let tags = [
        tag(FILE_ATTRIBUTES, &0x18u32.to_le_bytes()),
        tag(METADATA, xmp.as_bytes()),
    ];
    let (_, metadata) = extract(&movie(10, &tags));
    assert_eq!(metadata["dc:title"], ["Lighthouse"]);
    assert_eq!(metadata["dc:creator"], ["Anna Berg"]);
    assert_eq!(metadata["swf:version"], ["10"]);
    assert_eq!(metadata["swf:frameRate"], ["24"]);
    assert_eq!(metadata["swf:frameCount"], ["1"]);
    assert_eq!(metadata["swf:actionScript3"], ["true"]);
    assert!(!metadata.contains_key(EXCEPTION_WARNING));

    let (_, metadata) = extract(&movie(10, &[tag(METADATA, b"<x:xmpmeta>\0")]));
    assert!(
        metadata[EXCEPTION_WARNING][0].contains("does not read the movie's XMP metadata"),
        "{:?}",
        metadata[EXCEPTION_WARNING]
    );
}

#[test]
fn hands_images_and_mp3_sounds_to_the_embedded_document_extractor() {
    let jpeg = grey_jpeg();
    let (tables, image) = jpeg.split_at(jpeg.len() - 13);
    let tables = [tables, &[0xFF, 0xD9]].concat();
    let image = [&[0xFF, 0xD8][..], image].concat();
    let frame = [&[0xFF, 0xFB, 0x90, 0xC4][..], &[0; 413]].concat();
    let mp3 = frame.repeat(2);
    let tags = [
        tag(
            DEFINE_BITS_JPEG_2,
            &[
                &20u16.to_le_bytes()[..],
                &[0xFF, 0xD9, 0xFF, 0xD8],
                &jpeg[..],
            ]
            .concat(),
        ),
        tag(
            DEFINE_BITS_JPEG_3,
            &[
                &21u16.to_le_bytes()[..],
                &u32::try_from(jpeg.len()).unwrap().to_le_bytes(),
                &jpeg[..],
                &[0x78, 0x9C],
            ]
            .concat(),
        ),
        tag(JPEG_TABLES, &tables),
        tag(
            DEFINE_BITS,
            &[&22u16.to_le_bytes()[..], &image[..]].concat(),
        ),
        tag(
            DEFINE_SOUND,
            &[
                &23u16.to_le_bytes()[..],
                &[0x2E],
                &2304u32.to_le_bytes(),
                &[0, 0],
                &mp3[..],
            ]
            .concat(),
        ),
        tag(
            DEFINE_SOUND,
            &[
                &24u16.to_le_bytes()[..],
                &[0x1E],
                &4u32.to_le_bytes(),
                &[0; 8],
            ]
            .concat(),
        ),
        tag(DEFINE_BITS_LOSSLESS, &[25, 0, 3, 1, 0, 1, 0, 0, 0x78, 0x9C]),
        tag(SOUND_STREAM_BLOCK, &[0; 4]),
    ];
    let (body, metadata) = extract(&movie(10, &tags));
    for name in [
        "image-20.jpg",
        "image-21.jpg",
        "image-22.jpg",
        "sound-23.mp3",
    ] {
        assert!(body.contains(&format!("<h1>{name}</h1>")), "{name}: {body}");
    }
    assert!(!body.contains("sound-24"), "{body}");
    let warnings = &metadata[EXCEPTION_WARNING];
    for warning in [
        "not those in the sound format 1",
        "lossless bitmaps",
        "streamed sound",
    ] {
        assert!(
            warnings.iter().any(|given| given.contains(warning)),
            "{warning}: {warnings:?}"
        );
    }
}

#[test]
fn reads_the_tags_of_sprites_where_they_are_defined_and_warns_of_unknown_fonts() {
    let line = |font: u16, words: &str| {
        let run = Run {
            font: Some((font, 240)),
            x: Some(0),
            y: Some(100),
            glyphs: &glyphs(words),
        };
        text(DEFINE_TEXT, 10, &[run])
    };
    let sprite = [
        &30u16.to_le_bytes()[..],
        &1u16.to_le_bytes(),
        &line(1, "Inside")[..],
        &tag(END, &[])[..],
    ]
    .concat();
    let tags = [
        font_2(DEFINE_FONT_2, 1, &ascii()),
        tag(DEFINE_FONT, &[2, 0, 2, 0, 0x10, 0]),
        line(1, "Before"),
        tag(DEFINE_SPRITE, &sprite),
        line(1, "After"),
        line(9, "Imported"),
        line(2, "Uncoded"),
        line(9, "Imported"),
    ];
    let (body, metadata) = extract(&movie(10, &tags));
    assert_eq!(body, "<p>Before</p><p>Inside</p><p>After</p>");
    assert_eq!(
        metadata[EXCEPTION_WARNING],
        [
            "dijo::SwfParser does not write the text set in the font 9, which is not defined in \
             the movie",
            "dijo::SwfParser does not write the text set in the font 2, which has no code table",
        ]
    );
}

#[test]
fn fails_on_tags_that_claim_more_than_they_hold() {
    let truncated_text = tag(DEFINE_TEXT, &[10, 0]);
    let wide_entries = tag(DEFINE_TEXT, &[10, 0, 0, 0, 40, 11, 0]);
    let far_codes = {
        let mut font = font_2(DEFINE_FONT_2, 1, "ab");
        let offset = 2 + 2 + 3 + 5 + 2 + 2 * 2;
        font[offset..offset + 2].copy_from_slice(&0xFFF0u16.to_le_bytes());
        font
    };
    let past_the_font = text(
        DEFINE_TEXT,
        10,
        &[Run {
            font: Some((1, 240)),
            x: None,
            y: None,
            glyphs: &[2],
        }],
    );
    let inner = tag(DEFINE_SPRITE, &[31, 0, 1, 0]);
    let nested = tag(DEFINE_SPRITE, &[&[30, 0, 1, 0][..], &inner[..]].concat());
    let huge = [
        &(DEFINE_TEXT << 6 | 0x3F).to_le_bytes()[..],
        &u32::MAX.to_le_bytes(),
    ]
    .concat();
    let jpeg_3 = tag(DEFINE_BITS_JPEG_3, &[20, 0, 0xFF, 0, 0, 0, 0xFF, 0xD8]);
    let cases: [(Vec<Vec<u8>>, &str); 7] = [
        (
            vec![truncated_text],
            "DefineText tag at byte 5 of its body ends before its bounds",
        ),
        (vec![wide_entries], "packs glyph entries of 40-bit glyphs"),
        (
            vec![far_codes],
            "puts the code table of its 2 glyphs at offset 65520",
        ),
        (
            vec![font_2(DEFINE_FONT_2, 1, "ab"), past_the_font],
            "sets the glyph 2 of the font 1, which has 2 glyphs",
        ),
        (vec![nested], "is nested in another sprite"),
        (vec![huge], "declares 4294967295 bytes, more than the"),
        (
            vec![jpeg_3],
            "DefineBitsJPEG3 tag at byte 5 of its body ends before its image data",
        ),
    ];
    for (tags, reason) in cases {
        let file = movie(10, &tags);
        let error = failure(&file);
        assert!(error.contains(reason), "{reason}: {error}");
    }
    let mut cut = movie(10, &[]);
    cut.pop();
    let error = failure(&cut);
    assert!(
        error.contains("ends within the header of a tag at byte 5"),
        "{error}"
    );
}
