use super::*;

/// An em of a ten-point font, in the scaled points TeX's DVI files count in.
const EM: i32 = 655_360;
const SEVEN_POINT: i32 = 458_752;

/// The preamble TeX writes, with its `comment`.
fn preamble(comment: &str) -> Vec<u8> {
    let mut file = vec![PRE, ID];
    for number in [25_400_000u32, 473_628_672, 1000] {
        file.extend_from_slice(&number.to_be_bytes());
    }
    file.push(u8::try_from(comment.len()).unwrap());
    file.extend_from_slice(comment.as_bytes());
    file
}

/// The definition of the font `number` named `name` of the size `size`.
fn font(number: u8, name: &str, size: i32) -> Vec<u8> {
    let mut definition = vec![FNT_DEF1, number, 0, 0, 0, 0];
    definition.extend_from_slice(&size.to_be_bytes());
    definition.extend_from_slice(&size.to_be_bytes());
    definition.extend_from_slice(&[0, u8::try_from(name.len()).unwrap()]);
    definition.extend_from_slice(name.as_bytes());
    definition
}

fn right(by: i32) -> Vec<u8> {
    [&[RIGHT4][..], &by.to_be_bytes()].concat()
}

fn down(by: i32) -> Vec<u8> {
    [&[DOWN4][..], &by.to_be_bytes()].concat()
}

/// A page of `commands`.
fn page(commands: &[Vec<u8>]) -> Vec<u8> {
    [
        vec![BOP],
        vec![0; BOP_PARAMETERS],
        commands.concat(),
        vec![EOP],
    ]
    .concat()
}

/// A line of `commands` a baseline below the last, at the left margin.
fn line(commands: &[Vec<u8>]) -> Vec<u8> {
    [down(786_432), vec![PUSH], commands.concat(), vec![POP]].concat()
}

/// A file of the `fonts` defined and the `pages`, ended by its postamble.
fn dvi(fonts: &[Vec<u8>], pages: &[Vec<u8>]) -> Vec<u8> {
    [
        preamble(" TeX output 2026.11.13:1200"),
        fonts.concat(),
        pages.concat(),
        vec![POST],
    ]
    .concat()
}

/// The XHTML body and the metadata the native path extracts from the DVI file `file`.
fn extract(file: &[u8]) -> (String, dijo_core::Metadata) {
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(file)
        .unwrap();
    assert_eq!(metadata["Content-Type"], ["application/x-dvi"]);
    assert_eq!(metadata["tk:parsed-by"].last().unwrap(), "dijo::DviParser");
    let body =
        xml[xml.find("<body>").unwrap() + "<body>".len()..xml.find("</body>").unwrap()].to_owned();
    (body.replace(['\n', '\t'], ""), metadata)
}

fn failure(file: &[u8]) -> String {
    crate::Extractor::new()
        .extract_bytes_to_string(file)
        .unwrap_err()
        .to_string()
}

#[test]
fn writes_words_and_lines_by_how_the_characters_move() {
    let (body, metadata) = extract(&dvi(
        &[font(0, "cmr10", EM)],
        &[
            page(&[
                vec![FNT_NUM_0],
                line(&[
                    b"Hello".to_vec(),
                    right(EM / 3),
                    b"wor".to_vec(),
                    right(EM / 20),
                    b"ld".to_vec(),
                ]),
                line(&[b"Second".to_vec()]),
            ]),
            page(&[vec![FNT_NUM_0], line(&[b"Two".to_vec()])]),
        ],
    ));
    assert_eq!(
        body,
        "<div class=\"page\"><p>Hello world</p><p>Second</p></div>\
         <div class=\"page\"><p>Two</p></div>"
    );
    assert_eq!(metadata["xmpTPg:NPages"], ["2"]);
    assert_eq!(metadata["dvi:comment"], ["TeX output 2026.11.13:1200"]);
    assert_eq!(metadata["dcterms:created"], ["2026-11-13T12:00:00"]);
    assert!(!metadata.contains_key(EXCEPTION_WARNING));
}

#[test]
fn decodes_each_font_in_its_encoding_and_composes_accents() {
    let (body, metadata) = extract(&dvi(
        &[
            font(0, "cmr10", EM),
            font(1, "ecrm1000", EM),
            font(2, "cmex10", EM),
            font(3, "cmr7", SEVEN_POINT),
            font(4, "cmtt10", EM),
            font(5, "cmmi10", EM),
        ],
        &[page(&[
            line(&[
                vec![FNT_NUM_0],
                b"caf".to_vec(),
                right(-18_350),
                vec![0x13],
                right(-(EM / 2 - 18_350)),
                b"e".to_vec(),
                right(EM / 3),
                vec![0x0c],
                b"nd".to_vec(),
                right(EM / 3),
                vec![0x5c],
                b"quote".to_vec(),
                vec![0x22],
                right(EM / 3),
                vec![0x20],
                right(-EM / 2),
                b"lza".to_vec(),
            ]),
            line(&[
                vec![FNT_NUM_0],
                vec![PUSH],
                b"C".to_vec(),
                vec![POP, PUSH],
                right(100_000),
                vec![0x18],
                vec![POP],
                right(470_000),
                b"a".to_vec(),
                vec![FNT_NUM_0 + 1],
                right(EM / 3),
                b"K".to_vec(),
                vec![0xf8],
                b"benhavn".to_vec(),
                right(EM / 3),
                vec![0x8a, 0xf3],
                b"d".to_vec(),
                vec![0xba],
            ]),
            line(&[
                vec![FNT_NUM_0 + 5],
                b"x".to_vec(),
                vec![PUSH],
                down(-236_000),
                vec![FNT_NUM_0 + 3],
                b"2".to_vec(),
                vec![POP],
                right(240_000 + EM / 3),
                vec![FNT_NUM_0 + 2, 0x50],
                right(EM / 3),
                vec![FNT_NUM_0],
                b"end".to_vec(),
            ]),
            line(&[
                vec![FNT_NUM_0 + 4],
                b"a_b".to_vec(),
                right(EM / 2),
                b"{x}".to_vec(),
                vec![FNT_NUM_0, SET1, 200],
            ]),
        ])],
    ));
    assert_eq!(
        body,
        "<div class=\"page\"><p>caf\u{e9} find \u{201c}quote\u{201d} \u{142}za</p>\
         <p>C\u{327}a K\u{f8}benhavn \u{141}\u{f3}d\u{17a}</p><p>x2 end</p>\
         <p>a_b {x}\u{fffd}</p></div>"
    );
    assert_eq!(
        metadata[EXCEPTION_WARNING],
        [
            "dijo::DviParser does not know the encoding of the font cmex10: the characters set in \
          it are not written"
        ]
    );
}

#[test]
fn fails_a_file_that_is_not_a_dvi_file_or_breaks_its_rules() {
    let mut other_version = dvi(&[], &[]);
    other_version[1] = 7;
    assert!(failure(&other_version).contains("not of TeX's version 2"));
    let unended = dvi(&[font(0, "cmr10", EM)], &[page(&[vec![FNT_NUM_0, b'a']])]);
    assert!(failure(&unended[..unended.len() - 2]).contains("ends early"));
    let unpushed = dvi(&[], &[page(&[vec![POP]])]);
    assert!(failure(&unpushed).contains("pops a position it did not push"));
    let fontless = dvi(&[], &[page(&[b"a".to_vec()])]);
    assert!(failure(&fontless).contains("before it selects a font"));
    let undefined = dvi(&[], &[page(&[vec![FNT_NUM_0 + 9]])]);
    assert!(failure(&undefined).contains("selects the font 9"));
}

#[test]
fn knows_the_encodings_of_fonts_by_their_names() {
    for (name, encoding) in [
        ("cmr10", Some(Encoding::Ot1)),
        ("cmbx12", Some(Encoding::Ot1)),
        ("cmti10", Some(Encoding::Ot1Italic)),
        ("cmtt8", Some(Encoding::Ot1Typewriter)),
        ("cmmi7", Some(Encoding::Oml)),
        ("cmsy10", Some(Encoding::Oms)),
        ("ecrm1000", Some(Encoding::T1)),
        ("ectt1200", Some(Encoding::T1)),
        ("ec-lmr10", Some(Encoding::T1)),
        ("rm-lmr10", Some(Encoding::Ot1)),
        ("rm-lmri10", Some(Encoding::Ot1Italic)),
        ("rm-lmtt10", Some(Encoding::Ot1Typewriter)),
        ("ptmr8t", Some(Encoding::T1)),
        ("ptmri7t", Some(Encoding::Ot1Italic)),
        ("pcrr7t", Some(Encoding::Ot1Typewriter)),
        ("cmex10", None),
        ("msbm10", None),
        ("tcrm1000", None),
    ] {
        assert_eq!(Encoding::of_font(name), encoding, "{name}");
    }
    assert_eq!(Encoding::Ot1Italic.glyph(0x24), Some(Glyph::Text("\u{a3}")));
    assert_eq!(Encoding::Ot1.glyph(0x24), Some(Glyph::Text("$")));
    assert_eq!(Encoding::T1.glyph(0xdf), Some(Glyph::Text("SS")));
    assert_eq!(Encoding::Oml.glyph(0x3d), Some(Glyph::Text("/")));
    assert_eq!(Encoding::Ot1.glyph(0x80), None);
    assert!(
        matches!(Encoding::T1.glyph(0x00), Some(Glyph::Accent(accent)) if accent.mark == '\u{300}')
    );
}

#[test]
fn reads_the_date_tex_writes_in_its_comment() {
    assert_eq!(
        tex_date(" TeX output 1999.01.02:0304").as_deref(),
        Some("1999-01-02T03:04:00")
    );
    assert_eq!(tex_date(" TeX output 1999.13.02:0304"), None);
    assert_eq!(tex_date(" written by hand"), None);
}
