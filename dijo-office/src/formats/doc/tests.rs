use super::*;

#[test]
fn decrypts_an_encrypted_document_with_its_password_only() {
    let input = std::fs::read("../test_files/documents/encrypted/tika-password.doc").unwrap();
    let configs = |password: &str| {
        ParserConfigs::default().with_password(Some(&crate::config::Password::new(password)))
    };
    let opened = configs("tika");
    let (file, _, _) = plan(&input, &opened).unwrap();
    let mut text = String::new();
    planned(read(&file, &opened, &mut |out| {
        if let Out::Text(units) = out {
            text.push_str(&String::from_utf16_lossy(&units));
        }
        Ok::<(), Infallible>(())
    }))
    .unwrap();
    assert!(
        text.contains("This is an encrypted Word 2007 File."),
        "{text}"
    );
    for configs in [ParserConfigs::default(), configs("wrong")] {
        assert!(matches!(
            plan(&input, &configs),
            Err(Decline::Fails(crate::Error::Encrypted(_)))
        ));
    }
}

/// A Word 97 document whose text is `text` in code page 1252, of one piece, one character run
/// and one paragraph run of style 0 over all of it, with no styles and no fonts.
fn word97(text: &[u8]) -> Vec<u8> {
    word97_with_paragraph_run(text, text.len(), 0)
}

/// A Word 97 document whose text is `text` in code page 1252, of one piece, one character run
/// over all of it and one paragraph run of the style `istd` over its first `run` bytes, with no
/// styles and no fonts.
fn word97_with_paragraph_run(text: &[u8], run: usize, istd: u8) -> Vec<u8> {
    use crate::ole2::{write, Node, Storage};
    const TEXT_AT: usize = 1024;
    let put = |bytes: &mut Vec<u8>, at: usize, value: i32| {
        bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
    };
    let length = text.len() as i32;
    let mut main = vec![0; TEXT_AT];
    main[2..4].copy_from_slice(&193u16.to_le_bytes());
    main[0xA..0xC].copy_from_slice(&fib::TABLE_ONE_FLAG.to_le_bytes());
    put(&mut main, 64 + 0xC, length);
    main[152..154].copy_from_slice(&93u16.to_le_bytes());
    for (field, offset, size) in [
        (fib::field::DOP, 0, 500),
        (fib::field::STSHF, 500, 6),
        (fib::field::STTBFFFN, 512, 4),
        (fib::field::CLX, 520, 21),
        (fib::field::PLCFBTECHPX, 544, 12),
        (fib::field::PLCFBTEPAPX, 556, 12),
    ] {
        put(&mut main, 154 + field * 8, offset);
        put(&mut main, 154 + field * 8 + 4, size);
    }
    main.extend_from_slice(text);
    main.resize(main.len().next_multiple_of(512), 0);
    let character_page = main.len();
    main.resize(character_page + 1024, 0);
    let paragraph_page = character_page + 512;
    for (page, end) in [(character_page, length), (paragraph_page, run as i32)] {
        put(&mut main, page, TEXT_AT as i32);
        put(&mut main, page + 4, TEXT_AT as i32 + end);
        main[page + 511] = 1;
    }
    main[paragraph_page + 8] = 0x40;
    main[paragraph_page + 0x81] = 1;
    main[paragraph_page + 0x82] = istd;

    let mut table = vec![0; 568];
    table[500..506].copy_from_slice(&[4, 0, 0, 0, 10, 0]);
    table[520] = 2;
    put(&mut table, 521, 16);
    put(&mut table, 525, 0);
    put(&mut table, 529, length);
    put(&mut table, 535, (TEXT_AT as i32 * 2) | 0x4000_0000);
    for (bin, page) in [(544, character_page), (556, paragraph_page)] {
        put(&mut table, bin, TEXT_AT as i32);
        put(&mut table, bin + 4, TEXT_AT as i32 + length);
        put(&mut table, bin + 8, (page / 512) as i32);
    }
    let units = |name: &str| name.encode_utf16().collect::<Vec<u16>>();
    write(&Storage {
        class_id: [0; 16],
        entries: vec![
            (units("WordDocument"), Node::Owned(main)),
            (units("1Table"), Node::Owned(table)),
        ],
    })
    .unwrap()
}

/// A Word document too large to read whole is read in place, its paragraphs read from the main
/// stream one at a time and written a piece at a time, in memory that does not grow with it.
#[test]
fn extracts_documents_too_large_to_read_whole_in_bounded_memory() {
    use crate::input::MAX_READ_WHOLE;
    use dijo_core::large::LargeFile;
    use std::io::Write as _;

    let mut text = Vec::new();
    let mut lines = 0u64;
    let mut paragraph_start = 0;
    while text.len() as u64 <= MAX_READ_WHOLE {
        write!(text, "Linje {lines} i logbogen").unwrap();
        lines += 1;
        if text.len() - paragraph_start > 1 << 16 {
            text.push(b'\r');
            paragraph_start = text.len();
        } else {
            text.push(0x0B);
        }
    }
    *text.last_mut().unwrap() = b'\r';
    let document = word97(&text);
    drop(text);
    let file = LargeFile::new("large.doc", &document, 0, |_| Vec::new(), b"");
    drop(document);
    let last = format!("Linje {} i logbogen", lines - 1);
    let (mut first_seen, mut last_seen) = (false, false);
    let metadata = file.extract_lines("dijo::OfficeParser", |line| match line.trim() {
        "Linje 0 i logbogen" => first_seen = true,
        line if line == last => last_seen = first_seen,
        _ => {}
    });
    assert!(first_seen && last_seen);
    assert_eq!(metadata["Content-Type"], ["application/msword"]);
}

/// A million short paragraphs are held in about four bytes each, where their runs end in the
/// order they are read: a document of them, made too large to read whole by long paragraphs
/// after them, is extracted under the 16 MiB budget.
#[test]
fn extracts_a_million_short_paragraphs_in_bounded_memory() {
    use crate::input::MAX_READ_WHOLE;
    use dijo_core::large::LargeFile;
    use std::io::Write as _;

    const SHORT: u64 = 1_000_000;
    let mut text = Vec::new();
    for paragraph in 0..SHORT {
        write!(text, "Afsnit {paragraph}\r").unwrap();
    }
    let mut paragraph_start = text.len();
    let mut line = 0u64;
    while text.len() as u64 <= MAX_READ_WHOLE {
        write!(text, "Linje {line} i logbogen").unwrap();
        line += 1;
        if text.len() - paragraph_start > 1 << 16 {
            text.push(b'\r');
            paragraph_start = text.len();
        } else {
            text.push(0x0B);
        }
    }
    *text.last_mut().unwrap() = b'\r';
    let document = word97(&text);
    drop(text);
    let file = LargeFile::new("paragraphs.doc", &document, 0, |_| Vec::new(), b"");
    drop(document);
    let last_short = format!("Afsnit {}", SHORT - 1);
    let (mut short, mut last_seen) = (0u64, false);
    file.extract_lines("dijo::OfficeParser", |line| {
        let line = line.trim();
        if line.starts_with("Afsnit ") {
            short += 1;
            last_seen |= line == last_short;
        }
    });
    assert_eq!(short, SHORT);
    assert!(last_seen);
}

/// A paragraph whose mark lies past every paragraph run has no properties, as Word lays it
/// out, where POI's `PAPBinTable.rebuild` gives it the last run's again (#185).
#[test]
fn gives_paragraphs_past_every_run_no_properties() {
    let text = b"F\xf8rste afsnit\rAndet afsnit\rTredje afsnit\r";
    let first = text.iter().position(|&byte| byte == b'\r').unwrap() + 1;
    let input = word97_with_paragraph_run(text, first, 1);
    let configs = ParserConfigs::default();
    let file = office::open(&input, MEDIA_TYPE, "Word document").unwrap();
    let Ok(Word::Modern(streams)) = Word::read(&file, file.root(), &configs) else {
        panic!("a Word 97 document");
    };
    let paragraphs = streams.document().unwrap().paragraphs;
    assert_eq!(paragraphs.len(), 3);
    assert_eq!(paragraphs.past_runs, Some(1));
    let styles: Vec<i16> = (0..3)
        .map(|index| paragraphs.istd(index).unwrap())
        .collect();
    assert_eq!(styles, [1, 0, 0]);
    for index in [1, 2] {
        assert_eq!(
            paragraphs.sprms(index),
            Some(&bins::SprmBuffer::empty(2)),
            "{index}"
        );
    }
}
