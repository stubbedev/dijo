use super::story::Class;
use super::*;

/// `wordpro-bullets.lwp` is LibreOffice's `lotuswordpro/qa/cppunit/data/tdf33787-ordered-bullets.lwp`,
/// which its footer dedicates to the public domain (CC0); `wordpro-features.lwp` is LibreOffice's
/// `lotuswordpro/qa/cppunit/data/paragraphProperties.lwp` (MPL-2.0), saved by Word Pro 9.6.
fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!("../test_files/documents/{name}")).unwrap()
}

/// The blocks and the metadata of the document `input`.
fn read(input: &[u8]) -> Result<(Vec<Block>, TikaMetadata), Malformed> {
    let data = stream(input)?;
    let objects = Objects::read(&data)?;
    let text = story::read(&objects)?;
    let mut metadata = TikaMetadata::default();
    properties(&objects, &text, &mut metadata)?;
    Ok((text.blocks, metadata))
}

fn paragraph(text: &str) -> Block {
    Block::Paragraph(text.into())
}

#[test]
fn reads_the_header_the_body_and_the_footer() {
    let (blocks, _) = read(&fixture("wordpro-bullets.lwp")).unwrap();
    assert_eq!(
        blocks.first(),
        Some(&Block::Division(
            Class::Header,
            vec![paragraph(
                "Lotus Word Pro \u{2019}97 .lwp Bullet Points Example"
            )]
        ))
    );
    assert_eq!(blocks[1], paragraph("Numbered bullet one (1)"));
    assert_eq!(blocks[17], paragraph("Skipped bullet"));
    assert_eq!(blocks.len(), 26);
    assert_eq!(
        blocks.last(),
        Some(&Block::Division(
            Class::Footer,
            vec![
                paragraph("Jaxson Lee 2011"),
                paragraph("CC0 (http://creativecommons.org/publicdomain/zero/1.0/)")
            ]
        ))
    );
}

#[test]
fn reads_the_document_data_as_metadata() {
    let (_, metadata) = read(&fixture("wordpro-bullets.lwp")).unwrap();
    assert_eq!(metadata.values("dc:creator"), ["Davy"]);
    assert_eq!(metadata.values("dcterms:created"), ["2011-01-31T17:42:30Z"]);
    assert_eq!(
        metadata.values("dcterms:modified"),
        ["2022-07-15T00:16:59Z"]
    );
    assert!(metadata.values("meta:keyword").is_empty());
}

#[test]
fn reads_tables_and_footnotes_where_they_are_anchored() {
    let (blocks, metadata) = read(&fixture("wordpro-features.lwp")).unwrap();
    assert_eq!(metadata.values("dc:creator"), ["fong"]);
    let at = |block: &Block| blocks.iter().position(|found| found == block).unwrap();
    let footnote = Block::Division(Class::Footnote, vec![paragraph("This is a TrueType font")]);
    assert_eq!(
        at(&footnote),
        at(&paragraph("This line is Courier New.")) + 1
    );
    let table = &blocks[at(&paragraph("This it a 4*5 tables")) + 1];
    let Block::Table(rows) = table else {
        panic!("{table:?} is no table");
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        [vec![paragraph("row1")], vec![paragraph("column1")]]
    );
    assert_eq!(blocks.last(), Some(&paragraph("New Page")));
}

/// A small file keeps its `WordProData` stream imploded: the same document, its stream
/// imploded as literals, reads the same.
#[test]
fn reads_a_compressed_small_file() {
    let input = fixture("wordpro-bullets.lwp");
    let mut small = input[..STREAM_START].to_vec();
    small.extend(implode_literals(&input[STREAM_START..]));
    assert_eq!(read(&small).unwrap(), read(&input).unwrap());
}

/// `data` imploded as literals alone, then the end code.
fn implode_literals(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x00, 0x06];
    let mut held = 0u32;
    let mut count = 0;
    let mut put = |out: &mut Vec<u8>, value: u32, bits: u32| {
        held |= value << count;
        count += bits;
        while count >= 8 {
            out.push(held as u8);
            held >>= 8;
            count -= 8;
        }
    };
    for &byte in data {
        put(&mut out, 0, 1);
        put(&mut out, u32::from(byte), 8);
    }
    put(&mut out, 1, 1);
    for _ in 0..7 {
        put(&mut out, 0, 1);
    }
    put(&mut out, 0xFF, 8);
    put(&mut out, 0, 7);
    out
}

#[test]
fn leaves_a_document_before_word_pro_97_unread() {
    let mut input = fixture("wordpro-bullets.lwp");
    let revision = STREAM_START + 30 + 2;
    input[revision..revision + 2].copy_from_slice(&0x000Au16.to_le_bytes());
    let configs = crate::config::ParserConfigs::default();
    match WordProParser.prepare(&input, &TikaMetadata::default(), &configs) {
        Err(Decline::Unread(reason)) => assert!(reason.contains("0xa"), "{reason}"),
        Err(other) => panic!("{other:?}"),
        Ok(_) => panic!("a Word Pro 96 document is read"),
    };
}

/// Damage anywhere, or a file cut short, fails the document or reads less of it, but never
/// panics or loops.
#[test]
fn reads_damaged_documents_to_an_end() {
    let input = fixture("wordpro-features.lwp");
    for at in (0..input.len()).step_by(7) {
        let mut damaged = input.clone();
        damaged[at] ^= 0xA5;
        let _ = read(&damaged);
    }
    for length in (0..input.len()).step_by(97) {
        let _ = read(&input[..length]);
    }
}
