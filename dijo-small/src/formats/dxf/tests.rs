use super::*;

/// A value of a group of a binary drawing, of the type its code gives.
#[derive(Debug, Clone, Copy)]
enum Binary<'a> {
    Text(&'a [u8]),
    Real(f64),
    Int16(i16),
}

/// The ASCII drawing of `groups`, its codes right-aligned as AutoCAD writes them.
fn ascii(groups: &[(u16, &str)]) -> Vec<u8> {
    groups
        .iter()
        .flat_map(|(code, value)| format!("{code:>3}\n{value}\n").into_bytes())
        .collect()
}

/// The binary drawing of `groups`, its group codes in two bytes, or in one where `narrow`.
fn binary(groups: &[(u16, Binary)], narrow: bool) -> Vec<u8> {
    let mut drawing = b"AutoCAD Binary DXF\r\n\x1a\0".to_vec();
    for &(code, value) in groups {
        match (narrow, u8::try_from(code)) {
            (true, Ok(code)) if code != 255 => drawing.push(code),
            (true, _) => {
                drawing.push(255);
                drawing.extend_from_slice(&code.to_le_bytes());
            }
            (false, _) => drawing.extend_from_slice(&code.to_le_bytes()),
        }
        match value {
            Binary::Text(text) => {
                drawing.extend_from_slice(text);
                drawing.push(0);
            }
            Binary::Real(real) => drawing.extend_from_slice(&real.to_le_bytes()),
            Binary::Int16(integer) => drawing.extend_from_slice(&integer.to_le_bytes()),
        }
    }
    drawing
}

/// The groups of a drawing of the version `version` whose entities are `entities`.
fn drawing<'a>(version: &'a str, entities: &[(u16, &'a str)]) -> Vec<(u16, &'a str)> {
    let mut groups = vec![
        (0, "SECTION"),
        (2, "HEADER"),
        (9, "$ACADVER"),
        (1, version),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "ENTITIES"),
    ];
    groups.extend_from_slice(entities);
    groups.extend_from_slice(&[(0, "ENDSEC"), (0, "EOF")]);
    groups
}

/// The XHTML body and the metadata of the drawing `bytes`.
fn extract(bytes: &[u8]) -> (String, dijo_core::Metadata) {
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(bytes)
        .unwrap();
    assert_eq!(metadata["tk:parsed-by"].last().unwrap(), NAME);
    let body =
        xml[xml.find("<body>").unwrap() + "<body>".len()..xml.find("</body>").unwrap()].to_owned();
    (body, metadata)
}

/// The text `decoder` makes of `strings`, decoded from UTF-8 one after the other.
fn decoded(mut decoder: Decoder, strings: &[&str]) -> String {
    let mut out = String::new();
    for string in strings {
        let Ok(()) = decoder.decode(string.as_bytes(), Charset::named("UTF-8"), &mut out, |_| {
            Ok::<(), std::convert::Infallible>(())
        });
    }
    decoder.finish(&mut out);
    out
}

#[test]
fn writes_each_text_in_order_with_its_control_codes_read() {
    let groups = drawing(
        "AC1015",
        &[
            (0, "TEXT"),
            (8, "0"),
            (
                1,
                "%%c600 piles at 50% spacing, 45%%d %%p0.5 %%%%%u%%065%%u",
            ),
            (0, "LINE"),
            (1, "not text"),
            (0, "ATTDEF"),
            (1, "B-1"),
            (3, "Bollard number"),
            (2, "NUMBER"),
            (0, "ATTRIB"),
            (1, " B-7 "),
            (2, "NUMBER"),
            (0, "TEXT"),
            (1, ""),
        ],
    );
    let (body, metadata) = extract(&ascii(&groups));
    assert_eq!(metadata["Content-Type"], ["image/vnd.dxf; format=ascii"]);
    assert_eq!(metadata["dxf:version"], ["AC1015"]);
    assert_eq!(
        body,
        "<p>\u{2205}600 piles at 50% spacing, 45\u{b0} \u{b1}0.5 %A</p>\n\
         <p>B-1</p>\n<p> B-7 </p>\n"
    );
}

#[test]
fn joins_the_chunks_of_an_mtext_and_drops_its_formatting() {
    let groups = drawing(
        "AC1015",
        &[
            (0, "MTEXT"),
            (3, "{\\fArial|b1|i0|c0|p34;Mooring notes}\\"),
            (
                3,
                "PKeep \\S1/2; a fathom, \\S+0.1^-0.1; or \\S3#4;\\P\\LNo\\l ves",
            ),
            (
                1,
                "sel\\~here; P4\\\\B \\{x\\} {\\H1.5x;Depth^I4 m}\\Xbelow",
            ),
            (7, "Standard"),
            (0, "MTEXT"),
            (1, "\\pxqc;\\P\\P"),
        ],
    );
    let (body, _) = extract(&ascii(&groups));
    assert_eq!(
        body,
        "<p>Mooring notes</p>\n<p>Keep 1/2 a fathom, +0.1 -0.1 or 3/4</p>\n\
         <p>No vessel\u{a0}here; P4\\B {x} Depth\t4 m</p>\n<p>below</p>\n"
    );
}

#[test]
fn resolves_escapes_wherever_the_pieces_of_a_string_split_them() {
    assert_eq!(
        decoded(
            Decoder::new(Syntax::Text),
            &[
                "arrow \\U+21",
                "92, smile \\U+D83D\\U+DE00, \\M+182A0, lone \\U+D800x"
            ]
        ),
        "arrow \u{2192}, smile \u{1f600}, \u{3042}, lone \u{fffd}x"
    );
    assert_eq!(
        decoded(
            Decoder::new(Syntax::Text),
            &["\\U+12G4 \\M+6ABCD \\n %% %%z 50%"]
        ),
        "\\U+12G4 \\M+6ABCD \\n %% %%z 50%"
    );
    assert_eq!(
        decoded(Decoder::new(Syntax::Value), &["%%d ^Ia^Jb^ c^Q \\P {x}"]),
        "%%d \ta\nb^c^Q \\P {x}"
    );
    assert_eq!(
        decoded(Decoder::new(Syntax::Mtext), &["end \\U+00"]),
        "end \\U+00"
    );
    assert_eq!(
        decoded(
            Decoder::dimension("12.5".to_owned()),
            &["<> m, <x>, \\Pmax"]
        ),
        "12.5 m, <x>, \nmax"
    );
}

#[test]
fn writes_the_measurement_of_a_dimension_where_its_text_has_none() {
    let groups = drawing(
        "AC1015",
        &[
            (0, "DIMENSION"),
            (2, "*D1"),
            (70, "32"),
            (1, "<> m"),
            (42, "12.50000001"),
            (0, "DIMENSION"),
            (70, "34"),
            (42, "0.7853981633974483"),
            (0, "DIMENSION"),
            (1, " "),
            (42, "3.0"),
            (0, "DIMENSION"),
            (1, "TYP.\\Pclear"),
            (42, "100"),
            (0, "ARC_DIMENSION"),
            (42, "-0.00001"),
        ],
    );
    let (body, _) = extract(&ascii(&groups));
    assert_eq!(
        body,
        "<p>12.5 m</p>\n<p>45\u{b0}</p>\n<p>TYP.</p>\n<p>clear</p>\n<p>0</p>\n"
    );
}

#[test]
fn reads_blocks_but_not_those_dimensions_are_drawn_with() {
    let groups = [
        (0, "SECTION"),
        (2, "HEADER"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "BLOCKS"),
        (0, "BLOCK"),
        (2, "BOLLARD"),
        (1, ""),
        (0, "TEXT"),
        (1, "Bollard"),
        (0, "ENDBLK"),
        (0, "BLOCK"),
        (2, "*D1"),
        (0, "MTEXT"),
        (1, "12.5 m"),
        (0, "ENDBLK"),
        (0, "BLOCK"),
        (2, "*U2"),
        (0, "TEXT"),
        (1, "Anonymous"),
        (0, "ENDBLK"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "ENTITIES"),
        (0, "TEXT"),
        (1, "Pier"),
        (0, "ENDSEC"),
        (0, "EOF"),
        (0, "TEXT"),
        (1, "after the end"),
    ];
    let (body, _) = extract(&ascii(&groups));
    assert_eq!(body, "<p>Bollard</p>\n<p>Anonymous</p>\n<p>Pier</p>\n");
}

#[test]
fn decodes_the_code_page_before_autocad_2007_and_utf8_from_it_on() {
    let header = |version: &str, code_page: &str, text: &[u8]| {
        let mut bytes = ascii(&[
            (0, "SECTION"),
            (2, "HEADER"),
            (9, "$ACADVER"),
            (1, version),
            (9, "$DWGCODEPAGE"),
            (3, code_page),
            (0, "ENDSEC"),
            (0, "SECTION"),
            (2, "ENTITIES"),
            (0, "TEXT"),
        ]);
        bytes.extend_from_slice(b"  1\r\n");
        bytes.extend_from_slice(text);
        bytes.extend_from_slice(b"\r\n  0\r\nENDSEC\r\n  0\r\nEOF\r\n");
        bytes
    };
    let (body, _) = extract(&header("AC1015", "ANSI_1251", b"\xc4\xe0 \\U+00D8"));
    assert_eq!(body, "<p>\u{414}\u{430} \u{d8}</p>\n");
    let (body, _) = extract(&header("AC1018", "dos850", b"\x9d"));
    assert_eq!(body, "<p>\u{d8}</p>\n");
    let (body, _) = extract(&header("AC1032", "ANSI_1251", "\u{d8}".as_bytes()));
    assert_eq!(body, "<p>\u{d8}</p>\n");
    let without_header = [
        ascii(&[(0, "SECTION"), (2, "ENTITIES"), (0, "TEXT")]),
        b"  1\n\xd8\n  0\nENDSEC\n  0\nEOF\n".to_vec(),
    ]
    .concat();
    let (body, metadata) = extract(&without_header);
    assert_eq!(body, "<p>\u{d8}</p>\n");
    assert!(!metadata.contains_key("dxf:version"));
}

#[test]
fn reads_the_drawing_properties_and_dates() {
    let groups = [
        (0, "SECTION"),
        (2, "HEADER"),
        (9, "$ACADVER"),
        (1, "AC1024"),
        (9, "$TDUCREATE"),
        (40, "2461346.854166667"),
        (9, "$TDUUPDATE"),
        (40, "0.0"),
        (9, "$TDCREATE"),
        (40, "2461346.9"),
        (0, "ENDSEC"),
        (0, "SECTION"),
        (2, "OBJECTS"),
        (0, "XRECORD"),
        (100, "AcDbXrecord"),
        (280, "1"),
        (1, "Not the cookie"),
        (2, "Not a title"),
        (0, "XRECORD"),
        (5, "D0"),
        (100, "AcDbXrecord"),
        (280, "1"),
        (1, "DWGPROPS COOKIE"),
        (2, "Pier 4 Mooring Plan"),
        (3, "Moorings"),
        (4, "Anna Berg"),
        (6, ""),
        (7, "pier, \\U+00F8"),
        (8, "Ove Gr\u{f8}n"),
        (9, "3"),
        (300, "Harbour=Skagen"),
        (301, "="),
        (302, "Empty="),
        (40, "0.0"),
        (1, "https://example.org/pier"),
        (90, "0"),
        (0, "ENDSEC"),
        (0, "EOF"),
    ];
    let (body, metadata) = extract(&ascii(&groups));
    assert_eq!(body, "");
    assert_eq!(metadata["dxf:version"], ["AC1024"]);
    assert_eq!(metadata["dcterms:created"], ["2026-11-02T08:30:00Z"]);
    assert!(!metadata.contains_key("dcterms:modified"));
    assert_eq!(metadata["dc:title"], ["Pier 4 Mooring Plan"]);
    assert_eq!(metadata["dc:description"], ["Moorings"]);
    assert_eq!(metadata["dc:creator"], ["Anna Berg"]);
    assert_eq!(metadata["dc:subject"], ["pier, \u{f8}"]);
    assert_eq!(metadata["meta:last-author"], ["Ove Gr\u{f8}n"]);
    assert_eq!(metadata["dc:relation"], ["https://example.org/pier"]);
    assert_eq!(metadata["dwg-custom:Harbour"], ["Skagen"]);
    assert!(!metadata.contains_key("w:Comments"));
    assert!(!metadata.contains_key("dwg-custom:Empty"));
}

#[test]
fn reads_binary_drawings_of_one_and_two_byte_group_codes() {
    let groups = [
        (0, Binary::Text(b"SECTION")),
        (2, Binary::Text(b"HEADER")),
        (9, Binary::Text(b"$ACADVER")),
        (1, Binary::Text(b"AC1015")),
        (9, Binary::Text(b"$DWGCODEPAGE")),
        (3, Binary::Text(b"ANSI_1252")),
        (9, Binary::Text(b"$TDUCREATE")),
        (40, Binary::Real(2_461_346.854_166_667)),
        (0, Binary::Text(b"ENDSEC")),
        (0, Binary::Text(b"SECTION")),
        (2, Binary::Text(b"ENTITIES")),
        (0, Binary::Text(b"TEXT")),
        (10, Binary::Real(1.0)),
        (40, Binary::Real(2.5)),
        (1, Binary::Text(b"N\xf8rre Kaj")),
        (0, Binary::Text(b"MTEXT")),
        (3, Binary::Text(b"Line \\")),
        (1, Binary::Text(b"Pbreak")),
        (0, Binary::Text(b"DIMENSION")),
        (70, Binary::Int16(32)),
        (42, Binary::Real(12.5)),
        (1001, Binary::Text(b"ACAD")),
        (1070, Binary::Int16(1)),
        (0, Binary::Text(b"ENDSEC")),
        (0, Binary::Text(b"EOF")),
    ];
    for narrow in [false, true] {
        let (body, metadata) = extract(&binary(&groups, narrow));
        assert_eq!(metadata["Content-Type"], ["image/vnd.dxf; format=binary"]);
        assert_eq!(metadata["dcterms:created"], ["2026-11-02T08:30:00Z"]);
        assert_eq!(
            body, "<p>N\u{f8}rre Kaj</p>\n<p>Line </p>\n<p>break</p>\n<p>12.5</p>\n",
            "narrow: {narrow}"
        );
    }
}

#[test]
fn fails_on_drawings_it_cannot_read_to_their_end() {
    let mut cut_short = ascii(&drawing("AC1015", &[(0, "TEXT"), (1, "Pier")]));
    cut_short.truncate(cut_short.len() - "Pier\n  0\nENDSEC\n  0\nEOF\n".len());
    let not_a_code = [
        ascii(&[(0, "SECTION"), (2, "ENTITIES")]),
        b"x\nvalue\n".to_vec(),
    ]
    .concat();
    let unknown_code_page = ascii(&[
        (0, "SECTION"),
        (2, "HEADER"),
        (9, "$DWGCODEPAGE"),
        (3, "ANSI_9999"),
        (0, "ENDSEC"),
    ]);
    let not_a_version = ascii(&drawing("R2000", &[]));
    let not_a_number = ascii(&drawing("AC1015", &[(0, "DIMENSION"), (42, "twelve")]));
    let not_a_date = ascii(&[
        (0, "SECTION"),
        (2, "HEADER"),
        (9, "$TDUUPDATE"),
        (40, "12.5"),
        (0, "ENDSEC"),
    ]);
    let undefined_binary_code = binary(
        &[
            (0, Binary::Text(b"SECTION")),
            (2, Binary::Text(b"ENTITIES")),
            (80, Binary::Int16(0)),
        ],
        false,
    );
    let unended_binary_string = [
        binary(&[(0, Binary::Text(b"SECTION"))], false),
        b"\x02\0ENTITIES".to_vec(),
    ]
    .concat();
    for (what, drawing) in [
        ("cut short", cut_short),
        ("not a code", not_a_code),
        ("unknown code page", unknown_code_page),
        ("not a version", not_a_version),
        ("not a number", not_a_number),
        ("not a date", not_a_date),
        ("undefined binary code", undefined_binary_code),
        ("unended binary string", unended_binary_string),
    ] {
        let extracted = crate::Extractor::new().extract_bytes_to_string(&drawing);
        assert!(extracted.is_err(), "{what}: {extracted:?}");
    }
}

#[test]
fn names_the_charsets_of_autocads_code_pages() {
    for (code_page, charset) in [
        ("ANSI_1252", "windows-1252"),
        ("ANSI_932", "windows-31j"),
        ("ANSI_936", "GBK"),
        ("ANSI_949", "x-windows-949"),
        ("ANSI_950", "x-windows-950"),
        ("ANSI_1361", "x-Johab"),
        ("ANSI_874", "x-windows-874"),
        ("DOS437", "IBM437"),
        ("DOS932", "windows-31j"),
        ("ISO8859-2", "ISO-8859-2"),
        ("MACINTOSH", "x-MacRoman"),
        ("BIG5", "Big5"),
        ("GB2312", "GB2312"),
        ("KSC5601", "EUC-KR"),
        ("JOHAB", "x-Johab"),
    ] {
        let named = text::code_page(code_page).unwrap_or_else(|_| panic!("{code_page}"));
        assert_eq!(named.name(), charset, "{code_page}");
    }
    assert!(text::code_page("EBCDIC").is_err());
}

/// A drawing too large to read whole is read under a memory budget far below its size, an
/// `MTEXT` of more chunks than fit in it included.
#[test]
fn reads_drawings_too_large_to_read_whole_in_bounded_memory() {
    let chunk = |index: u64| format!("  3\nLine {index}\\P\n").into_bytes();
    let count = dijo_core::large::bodies_past_limit(chunk(0).len());
    let file = dijo_core::large::LargeFile::new(
        "long.dxf",
        &ascii(&[(0, "SECTION"), (2, "ENTITIES"), (0, "MTEXT")]),
        count,
        chunk,
        &ascii(&[(1, "Last"), (0, "ENDSEC"), (0, "EOF")]),
    );
    let mut lines = 0;
    file.extract_lines(DxfParser.name(), |line| {
        if line.is_empty() {
            return;
        }
        if lines == count {
            assert_eq!(line, "Last");
        } else {
            assert_eq!(line, format!("Line {lines}"));
        }
        lines += 1;
    });
    assert_eq!(lines, count + 1);
}
