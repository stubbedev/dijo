use super::*;

/// `harbour.parquet`, written by `dijo-small/tests/fixtures/parquet/harbour.py log`: two row
/// groups of three rows in data pages of version 1, dictionary-encoded, PLAIN and delta-encoded
/// strings with nulls, numbers, a date, a timestamp, a boolean, a decimal and a list.
const HARBOUR: &[u8] = include_bytes!("../../../../test_files/documents/harbour.parquet");
/// `pages-v2.parquet`, written by `harbour.py pages-v2`: one row group in data pages of version
/// 2, compressed with gzip, Brotli, LZ4_RAW, Zstandard and Snappy.
const PAGES_V2: &[u8] = include_bytes!("../../../tests/fixtures/parquet/pages-v2.parquet");

/// The XHTML body, without its line breaks and indentation, and the metadata the native path
/// extracts from `file`.
fn extract(file: &[u8]) -> (String, dijo_core::Metadata) {
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(file)
        .unwrap();
    assert_eq!(metadata["tk:parsed-by"].last().unwrap(), NAME);
    let body = crate::formats::testing::body(&xml).replace(['\n', '\t'], "");
    (body, metadata)
}

/// Why the extraction of `file` fails.
fn failure(file: &[u8]) -> String {
    crate::Extractor::new()
        .extract_bytes_to_string(file)
        .unwrap_err()
        .to_string()
}

/// A table of `rows` of cells, the first its head.
fn table(rows: &[&[&str]]) -> String {
    let mut table = String::from("<table>");
    for (index, row) in rows.iter().enumerate() {
        let cell = if index == 0 { "th" } else { "td" };
        table.push_str("<tr>");
        for text in *row {
            if text.is_empty() {
                table.push_str(&format!("<{cell} />"));
            } else {
                table.push_str(&format!("<{cell}>{text}</{cell}>"));
            }
        }
        table.push_str("</tr>");
    }
    table.push_str("</table>");
    table
}

#[test]
fn reads_the_rows_of_every_row_group_and_the_footer_metadata() {
    let (body, metadata) = extract(HARBOUR);
    let head: &[&str] = &[
        "berth",
        "vessel",
        "flag",
        "skipper",
        "remarks",
        "cargo",
        "length_m",
        "tonnage",
        "arrived",
        "moored_at",
        "paid",
        "fee",
        "crew.list.element",
    ];
    assert_eq!(
        body,
        table(&[
            head,
            &[
                "1",
                "Ellen Marie",
                "DK",
                "Jens Holm",
                "fenders split",
                "herring, salted",
                "21.5",
                "120",
                "2024-03-01",
                "2024-03-01T06:30:00Z",
                "true",
                "150.00",
                "Jens Ole",
            ],
            &[
                "2",
                "Havørn",
                "NO",
                "",
                "",
                "herring, smoked",
                "14.25",
                "45",
                "2024-03-02",
                "2024-03-02T18:05:12.250Z",
                "false",
                "75.50",
                "",
            ],
            &[
                "3",
                "Ellen Marie",
                "",
                "Jens Holm",
                "lines doubled at high water",
                "timber",
                "21.5",
                "118",
                "2024-03-02",
                "",
                "",
                "-0.05",
                "",
            ],
            &[
                "4",
                "Skagerrak",
                "DK",
                "Åse Lund",
                "waiting for the pilot",
                "timber, pine",
                "48.0",
                "1650",
                "1999-12-31",
                "1999-12-31T23:59:59Z",
                "true",
                "1234567.89",
                "Åse Karl Nils",
            ],
            &[
                "5",
                "Havørn",
                "NO",
                "",
                "left before dawn",
                "coal",
                "14.25",
                "46",
                "",
                "2024-03-05T04:00:00Z",
                "true",
                "",
                "Ida",
            ],
            &[
                "6",
                "Nordlys",
                "SE",
                "Pia Berg",
                "arrived under sail",
                "coal and coke",
                "1.0E-5",
                "-3",
                "2024-03-09",
                "2024-03-09T12:00:00Z",
                "false",
                "0.00",
                "Pia",
            ],
        ])
    );
    assert_eq!(metadata["Content-Type"], ["application/x-parquet"]);
    assert_eq!(
        metadata["xmp:CreatorTool"],
        ["parquet-cpp-arrow version 24.0.0"]
    );
    assert_eq!(metadata["database:row-count"], ["6"]);
    assert_eq!(metadata["database:column-count"], ["13"]);
    assert_eq!(metadata["database:column-name"], head);
    assert_eq!(metadata["parquet:harbour"], ["Skagen"]);
    assert_eq!(metadata["parquet:compiled-by"], ["the harbour master"]);
}

#[test]
fn reads_data_pages_of_version_2_in_every_codec() {
    let (body, _) = extract(PAGES_V2);
    let repeated = |word: &str| [word; 8].join(" ");
    let (skagen, hirtshals, grenen) = (
        repeated("Skagen"),
        repeated("Hirtshals"),
        repeated("Grenen"),
    );
    let (mile, vippefyr) = (repeated("Råbjerg Mile"), repeated("Vippefyr"));
    let (fyrtaarn, bundgarn) = (repeated("Fyrtårn"), repeated("Bundgarn"));
    assert_eq!(
        body,
        table(&[
            &[
                "gzip_text",
                "brotli_text",
                "lz4_text",
                "depth",
                "lit",
                "keeper_id",
                "lit_at",
                "logged",
                "lens",
                "position.lat",
                "position.lon",
            ],
            &[
                &skagen,
                &grenen,
                &vippefyr,
                "4.5",
                "true",
                "4294967295",
                "16:45:00",
                "2024-03-01T08:00:00.123456789",
                "1.5",
                "57.7366",
                "10.6325",
            ],
            &[
                &hirtshals,
                "",
                &fyrtaarn,
                "-0.25",
                "",
                "7",
                "00:00:00.000001",
                "",
                "",
                "",
                "",
            ],
            &[
                "",
                &mile,
                &bundgarn,
                "12.0",
                "false",
                "",
                "",
                "1970-01-01T00:00:00",
                "-2.0",
                "57.59",
                "",
            ],
        ])
    );
}

/// A value of compact Thrift.
enum T<'a> {
    I32(i32),
    I64(i64),
    Bin(&'a [u8]),
    List(u8, Vec<T<'a>>),
    Struct(Vec<(i16, T<'a>)>),
}

fn varint(mut value: u64, out: &mut Vec<u8>) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn zigzag(value: i64) -> u64 {
    ((value << 1) ^ (value >> 63)) as u64
}

impl T<'_> {
    fn code(&self) -> u8 {
        match self {
            T::I32(_) => 5,
            T::I64(_) => 6,
            T::Bin(_) => 8,
            T::List(..) => 9,
            T::Struct(_) => 12,
        }
    }

    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            T::I32(value) => varint(zigzag(i64::from(*value)), out),
            T::I64(value) => varint(zigzag(*value), out),
            T::Bin(bytes) => {
                varint(bytes.len() as u64, out);
                out.extend_from_slice(bytes);
            }
            T::List(element, items) => {
                if items.len() < 15 {
                    out.push((items.len() as u8) << 4 | element);
                } else {
                    out.push(0xF0 | element);
                    varint(items.len() as u64, out);
                }
                items.iter().for_each(|item| item.encode(out));
            }
            T::Struct(fields) => {
                let mut last = 0;
                for (id, value) in fields {
                    out.push(((id - last) as u8) << 4 | value.code());
                    value.encode(out);
                    last = *id;
                }
                out.push(0);
            }
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }
}

/// A data page of version 1 of `values`, PLAIN, of `count` of them, compressed into `body`.
fn data_page(count: i32, values: &[u8], body: &[u8]) -> Vec<u8> {
    let mut page = T::Struct(vec![
        (1, T::I32(0)),
        (2, T::I32(values.len() as i32)),
        (3, T::I32(body.len() as i32)),
        (
            5,
            T::Struct(vec![
                (1, T::I32(count)),
                (2, T::I32(0)),
                (3, T::I32(3)),
                (4, T::I32(3)),
            ]),
        ),
    ])
    .bytes();
    page.extend_from_slice(body);
    page
}

/// The INT32 values `values`, PLAIN.
fn plain(values: &[i32]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

/// A file of one required INT32 column `n` and one row group of `rows` rows, whose chunk is
/// `pages`, compressed with `codec`, said to take `size` bytes.
fn file_sized(rows: i64, codec: i32, pages: &[u8], size: i64) -> Vec<u8> {
    let schema = T::List(
        12,
        vec![
            T::Struct(vec![(4, T::Bin(b"schema")), (5, T::I32(1))]),
            T::Struct(vec![(1, T::I32(1)), (3, T::I32(0)), (4, T::Bin(b"n"))]),
        ],
    );
    let meta = T::Struct(vec![
        (1, T::I32(1)),
        (2, T::List(5, vec![T::I32(0)])),
        (3, T::List(8, vec![T::Bin(b"n")])),
        (4, T::I32(codec)),
        (5, T::I64(rows)),
        (6, T::I64(size)),
        (7, T::I64(size)),
        (9, T::I64(4)),
    ]);
    let chunk = T::Struct(vec![(2, T::I64(4)), (3, meta)]);
    let group = T::Struct(vec![
        (1, T::List(12, vec![chunk])),
        (2, T::I64(size)),
        (3, T::I64(rows)),
    ]);
    let footer = T::Struct(vec![
        (1, T::I32(1)),
        (2, schema),
        (3, T::I64(rows)),
        (4, T::List(12, vec![group])),
        (6, T::Bin(b"hand-written")),
    ])
    .bytes();
    frame(pages, &footer)
}

fn file(rows: i64, codec: i32, pages: &[u8]) -> Vec<u8> {
    file_sized(rows, codec, pages, pages.len() as i64)
}

/// A file of `pages` and the footer `footer`.
fn frame(pages: &[u8], footer: &[u8]) -> Vec<u8> {
    let mut file = MAGIC.to_vec();
    file.extend_from_slice(pages);
    file.extend_from_slice(footer);
    file.extend_from_slice(&(footer.len() as u32).to_le_bytes());
    file.extend_from_slice(MAGIC);
    file
}

fn problem(file: &[u8]) -> Problem {
    File::open(file).unwrap_err()
}

/// Whether `file` is malformed, as a reason holding `what` says.
fn malformed(file: &[u8], what: &str) -> bool {
    matches!(problem(file), Problem::Malformed(reason) if reason.contains(what))
}

#[test]
fn reads_pages_uncompressed_and_compressed() {
    let values = plain(&[7, -1]);
    let rows = table(&[&["n"], &["7"], &["-1"]]);
    let (body, metadata) = extract(&file(2, 0, &data_page(2, &values, &values)));
    assert_eq!(body, rows);
    assert_eq!(metadata["xmp:CreatorTool"], ["hand-written"]);
    let snappy = snap::raw::Encoder::new().compress_vec(&values).unwrap();
    assert_eq!(
        extract(&file(2, 1, &data_page(2, &values, &snappy))).0,
        rows
    );
}

#[test]
fn fails_columns_of_more_or_fewer_values_than_rows() {
    let values = plain(&[7, -1]);
    let short = file(3, 0, &data_page(2, &values, &values));
    assert!(failure(&short).contains("ends before its row group's rows"));
    let long = file(1, 0, &data_page(2, &values, &values));
    assert!(failure(&long).contains("more values than its row group's rows"));
    let cut = file(2, 0, &data_page(2, &values[..6], &values[..6]));
    assert!(failure(&cut).contains("the values end inside a value"));
    let wrong = file(2, 1, &data_page(2, &values, &values));
    assert!(failure(&wrong).contains("Snappy"));
}

#[test]
fn fails_files_that_are_not_parquet_or_whose_footers_are_hostile() {
    let values = plain(&[7]);
    let good = file(1, 0, &data_page(1, &values, &values));
    assert!(File::open(&good).is_ok());
    assert!(matches!(problem(&good[..10]), Problem::Malformed(_)));
    assert!(matches!(problem(&good[1..]), Problem::Malformed(_)));
    let mut encrypted = good.clone();
    let end = encrypted.len();
    encrypted[end - 4..].copy_from_slice(ENCRYPTED_MAGIC);
    assert!(matches!(problem(&encrypted), Problem::Encrypted(_)));
    let mut long_footer = good.clone();
    long_footer[end - 8..end - 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(problem(&long_footer), Problem::Malformed(_)));
    let lzo = file(1, 3, &data_page(1, &values, &values));
    assert!(matches!(problem(&lzo), Problem::Unsupported(reason) if reason.contains("LZO")));
    let past = file_sized(1, 0, &data_page(1, &values, &values), 1 << 20);
    assert!(matches!(problem(&past), Problem::Malformed(reason) if reason.contains("footer")));
    let unknown_codec = file(1, 99, &data_page(1, &values, &values));
    assert!(matches!(problem(&unknown_codec), Problem::Malformed(_)));
    let mut deep = vec![0x1C; thrift::MAX_DEPTH + 1];
    deep.extend(vec![0; thrift::MAX_DEPTH + 2]);
    assert!(malformed(&frame(&[], &deep), "deeper"));
    let mut wide = vec![0x29, 0xFC];
    varint(1_000_000_000, &mut wide);
    wide.push(0);
    assert!(malformed(&frame(&[], &wide), "items"));
    let mut long_string = vec![0x68];
    varint(1_000_000, &mut long_string);
    long_string.extend(b"text");
    assert!(matches!(
        problem(&frame(&[], &long_string)),
        Problem::Malformed(_)
    ));
    assert!(matches!(
        problem(&frame(&[], &[0x15, 0x02])),
        Problem::Malformed(_)
    ));
}
