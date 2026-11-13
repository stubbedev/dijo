use super::bencode::{self, Malformed, Value, MAX_DEPTH};

/// A value to bencode.
enum B<'a> {
    I(i64),
    S(&'a [u8]),
    L(Vec<B<'a>>),
    D(Vec<(&'a str, B<'a>)>),
}

fn s(text: &str) -> B<'_> {
    B::S(text.as_bytes())
}

impl B<'_> {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            B::I(value) => out.extend_from_slice(format!("i{value}e").as_bytes()),
            B::S(bytes) => {
                out.extend_from_slice(format!("{}:", bytes.len()).as_bytes());
                out.extend_from_slice(bytes);
            }
            B::L(items) => {
                out.push(b'l');
                items.iter().for_each(|item| item.encode(out));
                out.push(b'e');
            }
            B::D(entries) => {
                out.push(b'd');
                for (key, value) in entries {
                    s(key).encode(out);
                    value.encode(out);
                }
                out.push(b'e');
            }
        }
    }

    fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.encode(&mut out);
        out
    }
}

/// The XHTML body and the metadata the native path extracts from the torrent `file`.
fn extract(file: &[u8]) -> (String, dijo_core::Metadata) {
    let (xml, metadata) = crate::Extractor::new()
        .set_xml_output(true)
        .extract_bytes_to_string(file)
        .unwrap();
    assert_eq!(metadata["Content-Type"], ["application/x-bittorrent"]);
    assert_eq!(
        metadata["tk:parsed-by"].last().unwrap(),
        "dijo::BitTorrentParser"
    );
    let body = crate::formats::testing::body(&xml).to_owned();
    (body.replace(['\n', '\t'], ""), metadata)
}

/// A torrent announced to the example tracker, of the `rest` of its fields and the `info`
/// dictionary.
fn torrent<'a>(rest: Vec<(&'a str, B<'a>)>, info: Vec<(&'a str, B<'a>)>) -> Vec<u8> {
    let mut entries = vec![("announce", s("http://tracker.example.org/announce"))];
    entries.extend(rest);
    entries.push(("info", B::D(info)));
    B::D(entries).bytes()
}

fn file<'a>(length: i64, path: &[&'a str]) -> B<'a> {
    B::D(vec![
        ("length", B::I(length)),
        ("path", B::L(path.iter().map(|&part| s(part)).collect())),
    ])
}

#[test]
fn writes_a_multi_file_torrent_without_its_padding_files_or_piece_hashes() {
    let (body, metadata) = extract(&torrent(
        vec![
            (
                "announce-list",
                B::L(vec![
                    B::L(vec![s("http://tracker.example.org/announce")]),
                    B::L(vec![s("udp://backup.example.org:6969"), s("")]),
                ]),
            ),
            ("comment", s("Charts & logs")),
            ("created by", s("mktorrent 1.1")),
            ("creation date", B::I(1_731_499_200)),
        ],
        vec![
            (
                "files",
                B::L(vec![
                    file(1024, &["charts", "north.png"]),
                    B::D(vec![
                        ("attr", s("p")),
                        ("length", B::I(512)),
                        ("path", B::L(vec![s(".pad"), s("512")])),
                    ]),
                    file(7, &["_____padding_file_0_"]),
                    B::D(vec![
                        ("length", B::I(0)),
                        ("path", B::L(vec![s("raw")])),
                        ("path.utf-8", B::L(vec![s("logs"), s("r\u{e5}.txt")])),
                    ]),
                ]),
            ),
            ("name", s("Harbour")),
            ("piece length", B::I(16384)),
            ("pieces", B::S(&[0xff; 20])),
        ],
    ));
    assert_eq!(
        body,
        "<h1>Harbour</h1><p>Charts &amp; logs</p><table><tbody>\
         <tr><td>charts/north.png</td><td>1024</td></tr>\
         <tr><td>logs/r\u{e5}.txt</td><td>0</td></tr></tbody></table>\
         <ul><li>http://tracker.example.org/announce</li>\
         <li>udp://backup.example.org:6969</li></ul>"
    );
    assert_eq!(metadata["dc:title"], ["Harbour"]);
    assert_eq!(metadata["dc:description"], ["Charts & logs"]);
    assert_eq!(metadata["xmp:CreatorTool"], ["mktorrent 1.1"]);
    assert_eq!(metadata["dcterms:created"], ["2024-11-13T12:00:00Z"]);
    assert_eq!(
        metadata["bittorrent:announce"],
        [
            "http://tracker.example.org/announce",
            "udp://backup.example.org:6969"
        ]
    );
}

#[test]
fn writes_a_single_file_torrent_by_its_name_in_the_charset_it_names() {
    let (body, metadata) = extract(&torrent(
        vec![("encoding", s("ISO-8859-1"))],
        vec![("length", B::I(42)), ("name", B::S(b"F\xe6r\xf8.txt"))],
    ));
    assert_eq!(
        body,
        "<h1>F\u{e6}r\u{f8}.txt</h1><table><tbody><tr><td>F\u{e6}r\u{f8}.txt</td><td>42</td></tr>\
         </tbody></table><ul><li>http://tracker.example.org/announce</li></ul>"
    );
    assert_eq!(metadata["dc:title"], ["F\u{e6}r\u{f8}.txt"]);
    assert!(!metadata.contains_key("dcterms:created"));
}

#[test]
fn writes_the_files_of_a_version_2_file_tree_by_their_paths() {
    let leaf = |length| {
        B::D(vec![(
            "",
            B::D(vec![
                ("length", B::I(length)),
                ("pieces root", B::S(&[0; 32])),
            ]),
        )])
    };
    let (body, _) = extract(&torrent(
        vec![],
        vec![
            (
                "file tree",
                B::D(vec![
                    ("a.txt", leaf(3)),
                    ("docs", B::D(vec![("deep", B::D(vec![("b.txt", leaf(5))]))])),
                ]),
            ),
            ("meta version", B::I(2)),
            ("name", s("tree")),
        ],
    ));
    assert_eq!(
        body,
        "<h1>tree</h1><table><tbody><tr><td>a.txt</td><td>3</td></tr>\
         <tr><td>docs/deep/b.txt</td><td>5</td></tr></tbody></table>\
         <ul><li>http://tracker.example.org/announce</li></ul>"
    );
}

#[test]
fn fails_a_torrent_whose_fields_are_not_of_their_types_before_writing() {
    let failure = |file: &[u8]| {
        crate::Extractor::new()
            .extract_bytes_to_string(file)
            .unwrap_err()
            .to_string()
    };
    let negative = torrent(vec![], vec![("length", B::I(-1)), ("name", s("x"))]);
    assert!(
        failure(&negative).contains("a file of the length -1"),
        "{}",
        failure(&negative)
    );
    let nameless = torrent(vec![], vec![("length", B::I(1))]);
    assert!(failure(&nameless).contains("has no name"));
    let unknown = torrent(
        vec![("encoding", s("no-such-charset"))],
        vec![("length", B::I(1)), ("name", s("x"))],
    );
    assert!(failure(&unknown).contains("no-such-charset"));
    let mut truncated = torrent(vec![], vec![("length", B::I(1)), ("name", s("x"))]);
    truncated.pop();
    assert!(failure(&truncated).contains("ends inside a value"));
}

#[test]
fn checks_a_document_whole_and_bounds_its_nesting() {
    assert_eq!(bencode::parse(b"i-42e"), Ok(Value::Integer(-42)));
    assert_eq!(bencode::parse(b"4:spam"), Ok(Value::Bytes(b"spam")));
    let cases: [(&[u8], &str); 8] = [
        (b"i042e", "not a decimal integer"),
        (b"i-0e", "not a decimal integer"),
        (b"i99999999999999999999e", "not a decimal integer"),
        (b"5:spam", "runs past its end"),
        (b"di1ei2ee", "is not a byte string"),
        (b"d1:ae", "after a key without its value"),
        (b"le4:spam", "follow its value"),
        (b"x", "starts no value"),
    ];
    for (input, reason) in cases {
        let Err(Malformed(found)) = bencode::parse(input) else {
            panic!("{input:?} parses");
        };
        assert!(found.contains(reason), "{input:?}: {found}");
    }
    let nested = |depth| [vec![b'l'; depth], vec![b'e'; depth]].concat();
    assert!(bencode::parse(&nested(MAX_DEPTH)).is_ok());
    let Err(Malformed(found)) = bencode::parse(&nested(MAX_DEPTH + 1)) else {
        panic!("nesting deeper than {MAX_DEPTH} parses");
    };
    assert!(found.contains("nest deeper"), "{found}");
}

#[test]
fn reads_the_entries_of_a_dictionary_in_place() {
    let Ok(Value::Dictionary(dictionary)) = bencode::parse(b"d1:ai1e1:bl1:x1:ye1:cd1:di2eee")
    else {
        panic!("the dictionary does not parse");
    };
    let keys: Vec<&[u8]> = dictionary.entries().map(|(key, _)| key).collect();
    assert_eq!(keys, [&b"a"[..], b"b", b"c"]);
    let Some(Value::List(list)) = dictionary.get("b") else {
        panic!("b is no list");
    };
    assert_eq!(
        list.iter().collect::<Vec<_>>(),
        [Value::Bytes(b"x"), Value::Bytes(b"y")]
    );
    let Some(Value::Dictionary(inner)) = dictionary.get("c") else {
        panic!("c is no dictionary");
    };
    assert_eq!(inner.get("d"), Some(Value::Integer(2)));
    assert_eq!(dictionary.get("z"), None);
}
